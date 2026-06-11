//! Nexus intent classifier (P1-14).
//!
//! Classifies user input into one of four intents using a keyword/rule lookup
//! table first ($0, no LLM call). Only escalates to a single cheap LLM call
//! when competing strong signals are detected — in practice the common path
//! never leaves the keyword tier.

/// The four intents the Nexus dispatcher (P1-15) routes to.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Input requests a tool action (run X, check Y, fetch Z).
    ToolRequest,
    /// Input queries or references prior conversation / stored facts.
    MemoryQuery,
    /// Input asks to schedule, remind, or automate something at a time.
    ScheduleRequest,
    /// Default: general conversation, no specialized routing.
    Chat,
}

impl std::fmt::Display for Intent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Intent::ToolRequest => "tool_request",
            Intent::MemoryQuery => "memory_query",
            Intent::ScheduleRequest => "schedule_request",
            Intent::Chat => "chat",
        };
        f.write_str(s)
    }
}

// ── Keyword tables ────────────────────────────────────────────────────────

static MEMORY_CUES: &[&str] = &[
    "remember", "do you remember", "recall", "what did i say", "what did i tell",
    "do you know my", "did i mention", "from last time", "earlier i said",
    "i told you", "you know that i", "what was", "what were",
];

static SCHEDULE_CUES: &[&str] = &[
    "remind me", "set a reminder", "schedule", "every day", "every morning",
    "every night", "every week", "at 9am", "at 9 am", "at noon", "at midnight",
    "tomorrow at", "next week", "recurring", "repeat daily", "repeat weekly",
    "in 5 minutes", "in an hour", "set an alarm",
];

// Tool cues: broad set covering natural-language tool requests.
// Deliberately generous — false positives route to the tool path which
// gracefully falls back to plain text; false negatives cause hallucination.
static TOOL_CUES: &[&str] = &[
    // Direct action verbs
    "run ", "execute ", "fetch ", "query ", "look up", "look up",
    "scan ", "ping ", "deploy ", "restart ", "search ",
    "send ", "create ", "update ", "delete ", "list ",
    // Status / health queries
    "what is the status", "what's the status",
    "show me the ", "show me my ", "show my ",
    "what's on my ", "what is on my ", "what's in my ", "what is in my ",
    "how is my ", "how are my ",
    // Time / calendar
    "what time", "what's the time", "current time",
    "what day", "today's date", "what date",
    "on my calendar", "my calendar", "my events", "my appointments",
    // Web / search
    "search the web", "search for ", "google ", "web search",
    "find me ", "look for ", "browse ", "open url",
    // Server / infrastructure
    "server status", "server health", "check server",
    "are my servers", "is my server", "container status",
    // Data / pantry / home
    "my pantry", "in my pantry", "what food", "grocery",
    "my transactions", "my spending", "my budget", "my finances",
    // Weather / commute
    "weather", "forecast", "commute", "traffic",
    // Generic "check" (exclude schedule context: "check logs" is a schedule cue)
    "check the ", "check my ",
    // News / stocks
    "latest news", "stock price", "market ", "headline",
];


/// Classify input using the keyword lookup table (no LLM call).
///
/// Returns the dominant intent, or `None` if no keyword matched (default → `Chat`).
fn keyword_classify(lower: &str) -> Option<Intent> {
    // Count strong signals per category
    let mem_hits = MEMORY_CUES.iter().filter(|&&c| lower.contains(c)).count();
    let sched_hits = SCHEDULE_CUES.iter().filter(|&&c| lower.contains(c)).count();
    let tool_hits = TOOL_CUES.iter().filter(|&&c| lower.contains(c)).count();

    // If only one category has hits and it's unambiguous, return immediately
    let max_hits = mem_hits.max(sched_hits).max(tool_hits);
    if max_hits == 0 {
        return None; // no keyword match → caller falls back to Chat
    }

    // Priority order: memory > schedule > tool > chat.
    // If competing signals, the higher-priority intent wins without an LLM call.
    if mem_hits > 0 { return Some(Intent::MemoryQuery); }
    if sched_hits > 0 { return Some(Intent::ScheduleRequest); }
    if tool_hits > 0 { return Some(Intent::ToolRequest); }

    None
}

/// Classify `input` into an `Intent`.
///
/// Uses the keyword table first (no LLM, $0). Falls back to `Intent::Chat`
/// when nothing matches. Currently no LLM escalation path — Phase 1 handles
/// ambiguity by defaulting to Chat (safe).
///
/// `async` is kept in the signature for future LLM escalation in later phases.
pub async fn classify(input: &str) -> Intent {
    if input.is_empty() {
        return Intent::Chat;
    }

    let lower = input.to_lowercase();
    keyword_classify(&lower).unwrap_or(Intent::Chat)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn test_memory_query_keywords() {
        let inputs = [
            "do you remember what I said?",
            "recall our last conversation",
            "what did i say about the config?",
            "do you know my preferences?",
        ];
        for input in &inputs {
            let intent = rt().block_on(classify(input));
            assert_eq!(intent, Intent::MemoryQuery, "Expected MemoryQuery for: {input}");
        }
    }

    #[test]
    fn test_schedule_request_keywords() {
        let inputs = [
            "remind me to check the logs tomorrow",
            "schedule a daily backup at 9am",
            "set a reminder every morning",
            "remind me in an hour",
        ];
        for input in &inputs {
            let intent = rt().block_on(classify(input));
            assert_eq!(intent, Intent::ScheduleRequest, "Expected ScheduleRequest for: {input}");
        }
    }

    #[test]
    fn test_tool_request_keywords() {
        let inputs = [
            // Original cues
            "run the deployment script",
            "what is the status of the pipeline?",
            "fetch the latest logs",
            "ping the server",
            // New natural-language cues
            "what time is it",
            "what's on my calendar today?",
            "search the web for cookie recipes",
            "check my server health",
            "what's in my pantry?",
            "show me my recent transactions",
            "weather today",
            "check the server status",
            "how is my commute?",
            "search for hiking trails near San Jose",
            "what's the stock price of AAPL",
            "latest news",
            "my budget this month",
        ];
        for input in &inputs {
            let intent = rt().block_on(classify(input));
            assert_eq!(intent, Intent::ToolRequest, "Expected ToolRequest for: {input}");
        }
    }

    #[test]
    fn test_schedule_beats_tool_when_both_match() {
        // "remind me" (schedule) + "check" (tool) → schedule wins (higher priority)
        let input = "remind me to check the logs tomorrow";
        let intent = rt().block_on(classify(input));
        assert_eq!(intent, Intent::ScheduleRequest, "Schedule should beat tool for: {input}");
    }

    #[test]
    fn test_chat_default_no_keywords() {
        let inputs = [
            "hello there",
            "how are you?",
            "thanks for your help",
            "that was great",
            "what do you think about this idea?",
        ];
        for input in &inputs {
            let intent = rt().block_on(classify(input));
            assert_eq!(intent, Intent::Chat, "Expected Chat for: {input}");
        }
    }

    #[test]
    fn test_empty_input_returns_chat() {
        let intent = rt().block_on(classify(""));
        assert_eq!(intent, Intent::Chat);
    }

    #[test]
    fn test_intent_display() {
        assert_eq!(Intent::ToolRequest.to_string(), "tool_request");
        assert_eq!(Intent::MemoryQuery.to_string(), "memory_query");
        assert_eq!(Intent::ScheduleRequest.to_string(), "schedule_request");
        assert_eq!(Intent::Chat.to_string(), "chat");
    }
}
