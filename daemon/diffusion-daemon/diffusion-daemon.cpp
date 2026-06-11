// DiffusionGemma persistent inference daemon.
//
// llama-diffusion-cli loads the model (~36s) and exits after one generation. For batch / pipeline
// workloads (code review, spec enrichment, digests) that is unacceptable. This daemon loads the model
// once and serves many HTTP requests, then UNLOADS the model after an idle timeout to release VRAM —
// the HTTP listener stays up the whole time, so the next request transparently reloads (no external
// process supervision needs to respawn it, and no caller has to spawn a subprocess).
//
// Design notes:
//   * The C++ side is deliberately dumb: load model, generate text, return raw text + timing. All prompt
//     construction (review template) and verdict parsing live in the Rust terminus layer so they can be
//     iterated without a C++ rebuild.
//   * Standard llama.cpp flags still apply (-m, -ngl, -t, -c, -ub, -b, --diffusion-eb ...): the daemon
//     reuses common_params_parse. Daemon-only knobs come from env vars (DGEM_HTTP_PORT, DGEM_BIND,
//     DGEM_IDLE_TIMEOUT_SECS) so no changes to common/arg.cpp are needed.
//   * One mutex serializes generation (the diffusion canvas is single-threaded); concurrent HTTP calls
//     queue behind it.
//
// Endpoints:
//   POST /generate  {"system": "...", "prompt": "...", "max_tokens": 1024}
//                -> 200 {"text","time_ms","tokens","blocks","model_load_ms","input_tokens"}
//                -> 400 on bad input, 503 on model-load failure (e.g. VRAM occupied)
//   GET  /status   -> {"running","model_loaded","uptime_secs","requests_served","last_request_secs_ago",
//                      "idle_timeout_secs","model_load_ms"}
//   GET  /health   -> 200 "ok"

#include "arg.h"
#include "chat.h"
#include "common.h"
#include "diffusion.h"
#include "llama.h"
#include "log.h"

#include "httplib.h"
#include <nlohmann/json.hpp>

#include <atomic>
#include <chrono>
#include <cstdlib>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

// NB: llama.cpp's common headers already define a global `using json = nlohmann::ordered_json;`.
// We reuse that in-scope alias rather than declaring our own (which would conflict).

// ---- daemon config (env-driven, non-secret) --------------------------------------------------------

static std::string env_str(const char * key, const std::string & def) {
    const char * v = std::getenv(key);
    return (v && *v) ? std::string(v) : def;
}
static int env_int(const char * key, int def) {
    const char * v = std::getenv(key);
    if (!v || !*v) { return def; }
    char * end = nullptr;
    long n = strtol(v, &end, 10);
    if (!end) { return def; }
    // Tolerate trailing whitespace/newline (common when env is templated or echoed into a unit file).
    while (*end == ' ' || *end == '\t' || *end == '\n' || *end == '\r') { end++; }
    return (*end == '\0') ? (int) n : def;
}

// ---- canvas trim (lifted verbatim from diffusion-cli.cpp) ------------------------------------------

// Trim a denoised canvas: cut at the first end-of-generation token, or (checkpoints often emit no stop
// token) at the onset of a repetition loop (a token recurring at stride 1-2 for >= 6 steps).
static size_t trim_canvas(const llama_vocab * vocab, const llama_token * canvas, size_t n) {
    size_t cut = n;
    for (size_t i = 0; i < n; i++) {
        if (llama_vocab_is_eog(vocab, canvas[i])) { cut = i; break; }
    }
    for (size_t i = 0; i + 1 < cut; i++) {
        bool loop = false;
        for (size_t stride = 1; stride <= 2 && !loop; stride++) {
            size_t reps = 0;
            for (size_t j = i; j + stride < n && canvas[j] == canvas[j + stride]; j += stride) { reps++; }
            loop = reps >= 6;
        }
        if (loop) { cut = i; break; }
    }
    return cut;
}

// ---- engine: holds the loaded model + diffusion params; reloadable for idle-unload -----------------

struct GenResult {
    std::string text;
    int64_t     time_ms       = 0;
    int64_t     model_load_ms = 0;   // >0 only on the call that triggered a (re)load
    int         input_tokens  = 0;
    int         out_tokens     = 0;
    int         blocks         = 0;
    bool        ok            = false;
    std::string error;
};

class Engine {
  public:
    explicit Engine(common_params params) : params_(std::move(params)) {}

    // These are read lock-free by the idle watcher and the /status handler, concurrently with
    // load/unload (which run under the caller's gen_mutex). Back them with atomics so those reads are
    // not a data race; the atomics are written under the lock and mirror ctx_'s liveness.
    bool model_loaded() const { return loaded_flag_.load(std::memory_order_acquire); }

    int64_t last_load_ms() const { return last_load_ms_.load(std::memory_order_relaxed); }

    // Ensure the model is loaded. Returns false (and sets err) on failure. Caller holds the gen mutex.
    bool ensure_loaded(std::string & err) {
        if (ctx_) { return true; }
        const int64_t t0 = ggml_time_us();

        llama_model_params model_params = llama_model_default_params();
        model_params.n_gpu_layers       = params_.n_gpu_layers;
        model_params.devices            = params_.devices.data();
        model_params.use_mmap           = params_.use_mmap;
        model_params.use_direct_io      = params_.use_direct_io;
        model_params.use_mlock          = params_.use_mlock;
        model_params.check_tensors      = params_.check_tensors;

        model_ = llama_model_load_from_file(params_.model.path.c_str(), model_params);
        if (!model_) { err = "failed to load model from " + params_.model.path; return false; }
        if (!llama_model_is_diffusion(model_)) {
            err = "model is not a diffusion model";
            llama_model_free(model_); model_ = nullptr; return false;
        }

        // canvas_length: prompt is followed by a fixed canvas of masked positions (DiffusionGemma).
        char canvas_str[32];
        canvas_length_ = 0;
        if (llama_model_meta_val_str(model_, "diffusion.canvas_length", canvas_str, sizeof(canvas_str)) >= 0) {
            canvas_length_ = strtol(canvas_str, nullptr, 10);
        }
        if (canvas_length_ > 0) {
            llama_diffusion_set_sc(model_, nullptr, /*use_sc*/ 0.0f, /*temp_inv*/ 1.0f, /*enabled*/ true);
        }

        llama_context_params ctx_params = llama_context_default_params();
        ctx_params.n_ctx           = params_.n_ctx;
        ctx_params.n_batch         = params_.n_batch;
        ctx_params.n_ubatch        = params_.n_ubatch;
        ctx_params.flash_attn_type = params_.flash_attn_type;
        ctx_params.no_perf         = params_.no_perf;
        ctx_params.type_k          = params_.cache_type_k;
        ctx_params.type_v          = params_.cache_type_v;

        ctx_ = llama_init_from_model(model_, ctx_params);
        if (!ctx_) {
            err = "failed to create context (VRAM?)";
            llama_model_free(model_); model_ = nullptr; return false;
        }
        llama_set_n_threads(ctx_, params_.cpuparams.n_threads, params_.cpuparams_batch.n_threads);

        vocab_          = llama_model_get_vocab(model_);
        chat_templates_ = common_chat_templates_init(model_, "");
        output_tokens_.assign(params_.n_ubatch, 0);

        setup_diffusion_params();

        const int64_t load_ms = (ggml_time_us() - t0) / 1000;
        last_load_ms_.store(load_ms, std::memory_order_relaxed);
        loaded_flag_.store(true, std::memory_order_release);
        LOG_INF("dgem-daemon: model loaded in %lld ms (canvas_length=%lld, eb=%d)\n",
                (long long) load_ms, (long long) canvas_length_, (int) use_eb_);
        return true;
    }

    void unload() {
        loaded_flag_.store(false, std::memory_order_release);  // publish "unloaded" before freeing
        if (ctx_)   { llama_free(ctx_);       ctx_ = nullptr; }
        if (model_) { llama_model_free(model_); model_ = nullptr; }
        chat_templates_.reset();
        vocab_ = nullptr;
        output_tokens_.clear();
        LOG_INF("dgem-daemon: model unloaded, VRAM released\n");
    }

    // Generate a reply for one independent (system, user) turn. No history accumulation. Caller holds mutex.
    GenResult generate(const std::string & system, const std::string & user, int max_tokens) {
        GenResult r;
        std::string err;
        const bool was_loaded = ctx_ != nullptr;
        if (!ensure_loaded(err)) { r.error = err; return r; }
        if (!was_loaded) { r.model_load_ms = last_load_ms_.load(std::memory_order_relaxed); }

        // Build the chat-formatted prompt from system+user only (independent turn).
        std::vector<common_chat_msg> messages;
        if (!system.empty()) { messages.push_back(make_msg("system", system)); }
        messages.push_back(make_msg("user", user));
        common_chat_templates_inputs inputs;
        inputs.messages              = messages;
        inputs.add_generation_prompt = true;
        const std::string formatted = common_chat_templates_apply(chat_templates_.get(), inputs).prompt;

        const int64_t t0 = ggml_time_us();

        std::vector<llama_token> prefix = common_tokenize(vocab_, formatted, true, true);
        r.input_tokens = (int) prefix.size();
        // The prefix must fit the context AND the ubatch: the non-canvas path writes the prefix into
        // output_tokens_ (sized n_ubatch), and the canvas path needs [prefix | canvas] in one ubatch.
        // Guarding both here prevents a heap overflow in run_non_canvas and a guaranteed-empty canvas run.
        const int cap = std::min((int) llama_n_ctx(ctx_), (int) output_tokens_.size());
        if (r.input_tokens >= cap) {
            r.error = "input too long (" + std::to_string(r.input_tokens) + " tokens; capacity " +
                      std::to_string(cap) + " = min(ctx, ubatch)). Reduce the prompt/diff size.";
            return r;
        }

        const std::string text = (canvas_length_ <= 0)
            ? run_non_canvas(prefix)
            : run_canvas(prefix, max_tokens, r.blocks);

        // Empty output is a failure, not a blank answer: surface it so callers can fall back rather than
        // treat "" as a real (rubber-stamp) response.
        if (text.empty()) {
            r.error = "model produced no output (input may leave no room for a full canvas block, or "
                      "generation failed). Reduce the prompt size or check the daemon log.";
            return r;
        }

        r.time_ms    = (ggml_time_us() - t0) / 1000;
        r.text       = text;
        r.out_tokens = (int) common_tokenize(vocab_, text, false, false).size();
        r.ok         = true;
        return r;
    }

  private:
    static common_chat_msg make_msg(const std::string & role, const std::string & content) {
        common_chat_msg m; m.role = role; m.content = content; return m;
    }

    // Non-canvas (Dream/LLaDA) single fixed-length pass.
    std::string run_non_canvas(std::vector<llama_token> & prefix) {
        const int n_input = (int) prefix.size();
        diff_params_.max_length = params_.n_ubatch;
        int32_t n_generated = 0;
        diffusion_generate(ctx_, prefix.data(), output_tokens_.data(), n_input, diff_params_, n_generated);
        if (n_generated <= n_input) { return ""; }
        return common_detokenize(
            vocab_, std::vector<llama_token>(output_tokens_.begin() + n_input, output_tokens_.begin() + n_generated),
            false);
    }

    // Canvas block-diffusion (DiffusionGemma): denoise a canvas per block, committing each to the prefix
    // until an end token, repetition loop, the block budget, or the ubatch limit.
    std::string run_canvas(std::vector<llama_token> & prefix, int max_tokens, int & blocks_run) {
        const int32_t max_ub   = std::min((int32_t) params_.n_ubatch, (int32_t) llama_n_ctx(ctx_));
        const int32_t cl       = (int32_t) canvas_length_;
        int           n_blocks = (max_tokens > 0) ? ((max_tokens + cl - 1) / cl) : 1;
        if (n_blocks < 1) { n_blocks = 1; }

        std::vector<llama_token> response;
        blocks_run = 0;
        for (int b = 0; b < n_blocks; b++) {
            const int32_t prefix_len = (int32_t) prefix.size();
            const int32_t max_length = prefix_len + cl;
            if (max_length > max_ub) { break; }  // out of ubatch room: keep what we have

            diff_params_.max_length = max_length;
            eb_params_.max_length   = max_length;

            int32_t n_generated = 0;
            if (use_eb_) {
                diffusion_generate_entropy_bound(ctx_, prefix.data(), output_tokens_.data(), prefix_len,
                                                 eb_params_, n_generated);
            } else {
                diffusion_generate(ctx_, prefix.data(), output_tokens_.data(), prefix_len, diff_params_,
                                   n_generated);
            }
            if (n_generated <= prefix_len) { break; }

            const llama_token * canvas = output_tokens_.data() + prefix_len;
            const size_t        cut    = trim_canvas(vocab_, canvas, (size_t) cl);
            response.insert(response.end(), canvas, canvas + cut);
            blocks_run++;
            if (cut < (size_t) cl) { break; }                       // answer complete
            prefix.insert(prefix.end(), canvas, canvas + cut);       // commit block, denoise next
        }
        return common_detokenize(vocab_, response, false);
    }

    void setup_diffusion_params() {
        char shift_logits_str[8];
        if (llama_model_meta_val_str(model_, "diffusion.shift_logits", shift_logits_str, sizeof(shift_logits_str)) >= 0) {
            diff_params_.shift_logits = (strcmp(shift_logits_str, "true") == 0);
        } else {
            diff_params_.shift_logits = (canvas_length_ == 0);
        }

        if (canvas_length_ > 0) {
            diff_params_.schedule           = DIFFUSION_TRANSFER_SCHEDULE_TIMESTEP_BASED;
            diff_params_.eps                = params_.diffusion.eps > 0 ? params_.diffusion.eps : 1e-3f;
            diff_params_.suppress_mask_token = true;
            diff_params_.self_conditioning   = true;
        } else {
            if (params_.diffusion.eps) {
                diff_params_.schedule = DIFFUSION_TRANSFER_SCHEDULE_TIMESTEP_BASED;
                diff_params_.eps      = params_.diffusion.eps;
            } else if (params_.diffusion.block_length) {
                diff_params_.schedule     = DIFFUSION_TRANSFER_SCHEDULE_BLOCK_BASED;
                diff_params_.block_length = params_.diffusion.block_length;
            }
        }

        diff_params_.mask_token_id    = llama_vocab_mask(vocab_);
        diff_params_.seed             = params_.sampling.seed;
        diff_params_.temperature      = params_.sampling.temp;
        diff_params_.steps            = params_.diffusion.steps;
        diff_params_.algorithm        = static_cast<diffusion_algorithm>(params_.diffusion.algorithm);
        diff_params_.top_p            = params_.sampling.top_p;
        diff_params_.top_k            = params_.sampling.top_k;
        diff_params_.visual_mode      = false;
        diff_params_.add_gumbel_noise = params_.diffusion.add_gumbel_noise;
        diff_params_.step_callback    = nullptr;  // no visual/animation in the daemon

        use_eb_ = canvas_length_ > 0 && params_.diffusion.eb_mode != 2;
        if (use_eb_) {
            auto meta_f = [&](const char * key, float def) -> float {
                char buf[32];
                return llama_model_meta_val_str(model_, key, buf, sizeof(buf)) >= 0 ? strtof(buf, nullptr) : def;
            };
            auto meta_i = [&](const char * key, int32_t def) -> int32_t {
                char buf[32];
                return llama_model_meta_val_str(model_, key, buf, sizeof(buf)) >= 0 ? (int32_t) strtol(buf, nullptr, 10) : def;
            };
            eb_params_.max_denoising_steps  = meta_i("diffusion.eb_max_steps", 48);
            eb_params_.t_min                = meta_f("diffusion.eb_t_min", 0.4f);
            eb_params_.t_max                = meta_f("diffusion.eb_t_max", 0.8f);
            eb_params_.entropy_bound        = meta_f("diffusion.eb_entropy_bound", 0.1f);
            eb_params_.stability_threshold  = meta_i("diffusion.eb_stability_threshold", 1);
            eb_params_.confidence_threshold = meta_f("diffusion.eb_confidence_threshold", 0.005f);
            if (params_.diffusion.eb_t_min         >= 0) { eb_params_.t_min               = params_.diffusion.eb_t_min; }
            if (params_.diffusion.eb_t_max         >= 0) { eb_params_.t_max               = params_.diffusion.eb_t_max; }
            if (params_.diffusion.eb_entropy_bound >= 0) { eb_params_.entropy_bound       = params_.diffusion.eb_entropy_bound; }
            if (params_.diffusion.eb_stability     >= 0) { eb_params_.stability_threshold = params_.diffusion.eb_stability; }
            if (params_.diffusion.eb_confidence    >= 0) { eb_params_.confidence_threshold = params_.diffusion.eb_confidence; }
            if (params_.diffusion.eb_max_steps     >  0) { eb_params_.max_denoising_steps  = params_.diffusion.eb_max_steps; }
            eb_params_.seed          = params_.sampling.seed;
            eb_params_.visual_mode   = false;
            eb_params_.step_callback = nullptr;

            int gpu_devs = 0;
            for (size_t i = 0; i < ggml_backend_dev_count(); i++) {
                const auto dt = ggml_backend_dev_type(ggml_backend_dev_get(i));
                if (dt == GGML_BACKEND_DEVICE_TYPE_GPU || dt == GGML_BACKEND_DEVICE_TYPE_IGPU) { gpu_devs++; }
            }
            if      (params_.diffusion.eb_kv_cache == 1) { eb_params_.kv_cache = true; }
            else if (params_.diffusion.eb_kv_cache == 2) { eb_params_.kv_cache = false; }
            else                                         { eb_params_.kv_cache = (gpu_devs <= 1); }
        }
    }

    common_params              params_;
    llama_model *              model_ = nullptr;
    llama_context *            ctx_   = nullptr;
    const llama_vocab *        vocab_ = nullptr;
    common_chat_templates_ptr  chat_templates_;
    int64_t                    canvas_length_ = 0;
    bool                       use_eb_        = false;
    diffusion_params           diff_params_;
    diffusion_eb_params        eb_params_;
    std::vector<llama_token>   output_tokens_;
    std::atomic<int64_t>       last_load_ms_{0};
    std::atomic<bool>          loaded_flag_{false};
};

// ---- main ------------------------------------------------------------------------------------------

int main(int argc, char ** argv) {
    std::setlocale(LC_NUMERIC, "C");
    ggml_time_init();

    common_params params;
    common_init();
    if (!common_params_parse(argc, argv, params, LLAMA_EXAMPLE_DIFFUSION)) { return 1; }

    llama_backend_init();

    const std::string bind_addr      = env_str("DGEM_BIND", "127.0.0.1");
    const int         port           = env_int("DGEM_HTTP_PORT", 8877);
    const int         idle_timeout_s = env_int("DGEM_IDLE_TIMEOUT_SECS", 300);

    Engine engine(params);
    std::mutex gen_mutex;  // serializes generation (single-threaded canvas)

    using clock = std::chrono::steady_clock;
    const auto start_time = clock::now();
    std::atomic<int64_t> requests_served{0};
    std::atomic<int64_t> last_activity_ms{
        std::chrono::duration_cast<std::chrono::milliseconds>(start_time.time_since_epoch()).count()};

    auto now_ms = [] {
        return std::chrono::duration_cast<std::chrono::milliseconds>(clock::now().time_since_epoch()).count();
    };

    httplib::Server svr;

    svr.Get("/health", [](const httplib::Request &, httplib::Response & res) {
        res.set_content("ok", "text/plain");
    });

    svr.Get("/status", [&](const httplib::Request &, httplib::Response & res) {
        const int64_t uptime = std::chrono::duration_cast<std::chrono::seconds>(clock::now() - start_time).count();
        const int64_t since  = (now_ms() - last_activity_ms.load()) / 1000;
        json j = {
            {"running", true},
            {"model_loaded", engine.model_loaded()},
            {"uptime_secs", uptime},
            {"requests_served", requests_served.load()},
            {"last_request_secs_ago", since},
            {"idle_timeout_secs", idle_timeout_s},
            {"model_load_ms", engine.last_load_ms()},
        };
        res.set_content(j.dump(), "application/json");
    });

    svr.Post("/generate", [&](const httplib::Request & req, httplib::Response & res) {
        json body;
        std::string system, prompt;
        int max_tokens = 1024;
        try {
            body = json::parse(req.body);
            // value() throws type_error for present-but-wrong-typed fields (e.g. max_tokens as a string);
            // keep it inside the try so a bad type yields 400, not an uncaught 500.
            system     = body.value("system", std::string());
            prompt     = body.value("prompt", std::string());
            max_tokens = body.value("max_tokens", 1024);
        } catch (const std::exception & e) {
            res.status = 400;
            res.set_content(json{{"error", std::string("invalid request: ") + e.what()}}.dump(), "application/json");
            return;
        }
        // Clamp the output budget to a sane range: guards against signed-overflow in the block math and
        // an unbounded generation loop from a hostile or mistaken max_tokens.
        if (max_tokens < 1)    { max_tokens = 1; }
        if (max_tokens > 8192) { max_tokens = 8192; }
        if (prompt.empty()) {
            res.status = 400;
            res.set_content(json{{"error", "missing 'prompt'"}}.dump(), "application/json");
            return;
        }

        GenResult r;
        {
            std::lock_guard<std::mutex> lock(gen_mutex);
            last_activity_ms.store(now_ms());          // mark active before the (possibly long) generation
            r = engine.generate(system, prompt, max_tokens);
            last_activity_ms.store(now_ms());          // and after, so idle timer starts from completion
        }
        if (!r.ok) {
            res.status = 503;
            res.set_content(json{{"error", r.error}}.dump(), "application/json");
            return;
        }
        requests_served.fetch_add(1);
        json j = {
            {"text", r.text},
            {"time_ms", r.time_ms},
            {"model_load_ms", r.model_load_ms},
            {"input_tokens", r.input_tokens},
            {"tokens", r.out_tokens},
            {"blocks", r.blocks},
        };
        res.set_content(j.dump(), "application/json");
    });

    // Idle-unload watcher: release VRAM after inactivity. The HTTP listener stays up; the next request
    // transparently reloads the model.
    std::atomic<bool> stop_watcher{false};
    std::thread watcher([&] {
        // Poll every 5s so idle-unload latency is at most idle_timeout + ~5s (responsive, negligible cost).
        while (!stop_watcher.load()) {
            for (int i = 0; i < 5 && !stop_watcher.load(); i++) {
                std::this_thread::sleep_for(std::chrono::seconds(1));
            }
            if (stop_watcher.load()) { break; }
            const int64_t idle_s = (now_ms() - last_activity_ms.load()) / 1000;
            if (engine.model_loaded() && idle_s >= idle_timeout_s) {
                std::lock_guard<std::mutex> lock(gen_mutex);
                // re-check under lock: a request may have arrived between the read and the lock
                if (engine.model_loaded() &&
                    (now_ms() - last_activity_ms.load()) / 1000 >= idle_timeout_s) {
                    engine.unload();
                }
            }
        }
    });

    LOG_INF("dgem-daemon: listening on %s:%d (idle_timeout=%ds, model='%s')\n",
            bind_addr.c_str(), port, idle_timeout_s, params.model.path.c_str());
    const bool ok = svr.listen(bind_addr.c_str(), port);

    stop_watcher.store(true);
    watcher.join();
    engine.unload();
    llama_backend_free();
    return ok ? 0 : 1;
}
