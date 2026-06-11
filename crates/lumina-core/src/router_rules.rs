//! FORGE-02: Category rule definitions for the three-layer model router.
//!
//! Pure functions — no I/O, no LLM calls. Every rule is case-insensitive.
//! Separated from router.rs for testability and future TOML configurability.

/// Estimate the token count of a text (word_count * 1.3 approximation).
pub fn estimate_tokens(text: &str) -> usize {
    let words = text.split_whitespace().count();
    (words as f64 * 1.3).ceil() as usize
}

/// Layer 2: Determine whether input needs the deep (120B) model.
///
/// Returns true if ANY escalation rule matches. The safety principle is
/// "when in doubt, escalate" — a wrong escalation costs a few seconds,
/// a missed escalation produces a bad answer.
pub fn needs_deep_reasoning(input: &str, token_threshold: usize) -> bool {
    let lower = input.to_lowercase();

    has_multi_step_markers(&lower)
        || has_code_markers(&lower)
        || has_math_markers(&lower)
        || has_reasoning_markers(&lower)
        || estimate_tokens(input) > token_threshold
}

/// Multi-step instructions: 3+ sequential transition words.
fn has_multi_step_markers(lower: &str) -> bool {
    let markers = ["first", "then", "next", "finally", "after that", "step 1", "step 2"];
    let count = markers.iter().filter(|&&m| lower.contains(m)).count();
    count >= 3
}

/// Code or programming request.
pub fn has_code_markers(lower: &str) -> bool {
    lower.contains("```")
        || lower.contains("write code")
        || lower.contains("write a function")
        || lower.contains("write a script")
        || lower.contains("debug")
        || lower.contains("function")
        || lower.contains("implement")
        || lower.contains("refactor")
        || lower.contains("class ")
        || lower.contains("method ")
        || lower.contains("compile")
        || lower.contains("syntax error")
        || lower.contains("cargo")
        || lower.contains("python")
        || lower.contains("javascript")
        || lower.contains("typescript")
        || lower.contains("rust ")
        || lower.contains("golang")
}

/// Math or quantitative reasoning.
pub fn has_math_markers(lower: &str) -> bool {
    lower.contains("calculate")
        || lower.contains("compute")
        || lower.contains("equation")
        || lower.contains("formula")
        || lower.contains("solve")
        || lower.contains("algebra")
        || lower.contains("integral")
        || lower.contains("derivative")
        || lower.contains("probability")
        || lower.contains("statistics")
}

/// Deep reasoning or analysis request.
pub fn has_reasoning_markers(lower: &str) -> bool {
    lower.contains("analyze")
        || lower.contains("analyse")
        || lower.contains("compare")
        || lower.contains("evaluate")
        || lower.contains("explain why")
        || lower.contains("pros and cons")
        || lower.contains("help me decide")
        || lower.contains("what should i")
        || lower.contains("recommend")
        || lower.contains("suggest strategy")
        || lower.contains("think through")
        || lower.contains("walk me through")
        || lower.contains("trade-off")
        || lower.contains("tradeoff")
        || lower.contains("architecture")
        || lower.contains("design pattern")
}

/// Layer 3: Detect uncertainty markers in a model response.
///
/// Returns true if the response contains hedging language that suggests
/// the model wasn't confident — triggering a re-route to the deep model.
pub fn has_uncertainty_markers(response: &str) -> bool {
    let lower = response.to_lowercase();
    lower.contains("i'm not sure")
        || lower.contains("i am not sure")
        || lower.contains("i think ")
        || lower.contains("i believe ")
        || lower.contains("this might be")
        || lower.contains("it's possible")
        || lower.contains("it is possible")
        || lower.contains("i don't have enough information")
        || lower.contains("i do not have enough information")
        || lower.contains("i'm not certain")
        || lower.contains("i am not certain")
        || lower.contains("it depends")
        || lower.contains("i may be wrong")
        || lower.contains("you might want to verify")
        || lower.contains("i cannot be sure")
        || lower.contains("i can't be sure")
}

/// Layer 1: Detect natural language "force deep" phrasing.
pub fn is_natural_deep_override(lower: &str) -> bool {
    lower.starts_with("think carefully")
        || lower.starts_with("think hard")
        || lower.starts_with("deep think")
        || lower.contains(" think carefully")
        || lower.contains("please think carefully")
}

/// Layer 1: Detect natural language "force fast" phrasing.
pub fn is_natural_fast_override(lower: &str) -> bool {
    lower.starts_with("quick answer")
        || lower.starts_with("just quickly")
        || lower.starts_with("quick:")
        || lower.starts_with("briefly,")
        || lower.contains("quick answer please")
        || lower.contains("just give me a quick")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens("hello world"), 3); // ceil(2 * 1.3) = 3
        assert_eq!(estimate_tokens(""), 0);
        let long: String = "word ".repeat(400);
        assert!(estimate_tokens(&long) > 500);
    }

    #[test]
    fn test_multi_step_detection() {
        assert!(needs_deep_reasoning("First do X, then do Y, finally do Z", 500));
        assert!(!needs_deep_reasoning("First and then", 500)); // only 2 markers
        assert!(needs_deep_reasoning("Step 1: install. Next, configure. Then deploy. Finally test.", 500));
    }

    #[test]
    fn test_code_detection() {
        assert!(needs_deep_reasoning("```rust\nfn main() {}\n```", 500));
        assert!(needs_deep_reasoning("Please debug this function", 500));
        assert!(needs_deep_reasoning("write code to parse JSON", 500));
        assert!(needs_deep_reasoning("implement a binary search", 500));
        assert!(needs_deep_reasoning("refactor this rust code", 500));
    }

    #[test]
    fn test_math_detection() {
        assert!(needs_deep_reasoning("calculate the integral of x^2", 500));
        assert!(needs_deep_reasoning("solve this equation: 2x + 3 = 7", 500));
        assert!(needs_deep_reasoning("compute the probability of A given B", 500));
    }

    #[test]
    fn test_reasoning_detection() {
        assert!(needs_deep_reasoning("analyze the pros and cons of this approach", 500));
        assert!(needs_deep_reasoning("help me decide between Postgres and MySQL", 500));
        assert!(needs_deep_reasoning("what should I use for this project", 500));
        assert!(needs_deep_reasoning("recommend a strategy for scaling", 500));
        assert!(needs_deep_reasoning("evaluate the trade-offs here", 500));
    }

    #[test]
    fn test_casual_stays_fast() {
        assert!(!needs_deep_reasoning("hello how are you", 500));
        assert!(!needs_deep_reasoning("what time is it", 500));
        assert!(!needs_deep_reasoning("thanks!", 500));
        assert!(!needs_deep_reasoning("ok got it", 500));
    }

    #[test]
    fn test_long_input_escalates() {
        let long: String = "word ".repeat(450); // > 500 tokens (450 * 1.3 ≈ 585)
        assert!(needs_deep_reasoning(&long, 500));
    }

    #[test]
    fn test_short_input_stays_fast() {
        let short = "hello world";
        assert!(!needs_deep_reasoning(short, 500));
    }

    #[test]
    fn test_uncertainty_markers() {
        assert!(has_uncertainty_markers("I'm not sure about this"));
        assert!(has_uncertainty_markers("I think the answer is 42"));
        assert!(has_uncertainty_markers("This might be the right approach"));
        assert!(has_uncertainty_markers("It depends on your use case"));
        assert!(has_uncertainty_markers("I don't have enough information to answer"));
        assert!(has_uncertainty_markers("I'm not certain, but it could be X"));

        assert!(!has_uncertainty_markers("The answer is 42."));
        assert!(!has_uncertainty_markers("Here's how to do it:"));
        assert!(!has_uncertainty_markers("Use Postgres for this workload."));
    }

    #[test]
    fn test_natural_overrides() {
        assert!(is_natural_deep_override("think carefully about this decision"));
        assert!(is_natural_deep_override("please think carefully before answering"));
        assert!(is_natural_fast_override("quick answer please"));
        assert!(is_natural_fast_override("just quickly, what's the capital of France"));
        assert!(!is_natural_deep_override("hello"));
        assert!(!is_natural_fast_override("analyze this deeply"));
    }
}
