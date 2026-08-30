//! Standing background an agent reads before the conversation starts.
//!
//! A gameplan says what the session is *for*; a context source says what it is
//! *about* — the layout of a repository, the rows of a table, the state of a
//! deployment. Both end up in the system prompt, and the difference is where
//! they come from: the gameplan is a file the harness author wrote, and a
//! context source is a function the host program supplies, called once per agent
//! at the top of the run.
//!
//! Called once, and the result is what every turn reads. A source that walks a
//! tree or queries a service therefore pays for that once per agent per run
//! rather than once per turn, and a source that fails does so before the first
//! provider call — the same bargain persona, skill, and tool resolution make.
//!
//! Sources are registered with
//! [`Session::add_context`](crate::session::Session::add_context) and narrowed
//! by a gameplan's `context:` key, exactly as tools are: a gameplan can name
//! fewer than were registered, never more, because a name nobody registered is
//! a name for nothing.
//!
//! The text goes into the prompt as the session's own material, with no quoting
//! caveat — unlike the memory file, which carries one because agents write it.
//! Whatever a source returns is what the program that started the session chose
//! to put in front of the model, so a source that renders untrusted input is
//! responsible for framing it.

use crate::error::Result;

/// A named block of standing text a session puts in an agent's system prompt.
///
/// The framework ships no implementation: what a session's agents need to know
/// about the world is exactly the part it cannot guess.
pub trait ContextSource: Send + Sync {
    /// The text *agent* should see, or an empty string to contribute nothing.
    ///
    /// *agent* is the name the agent was registered under, so one source can
    /// hand a reviewer and an author different views of the same subject.
    fn render(&self, agent: &str) -> Result<String>;
}

impl<F> ContextSource for F
where
    F: Fn(&str) -> Result<String> + Send + Sync,
{
    fn render(&self, agent: &str) -> Result<String> {
        self(agent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_closure_is_a_context_source() {
        // The blanket impl is what lets a caller pass a closure where the
        // signature asks for the trait, as the access approver does.
        let source: Arc<dyn ContextSource> =
            Arc::new(|agent: &str| Ok(format!("Repository map for {agent}")));
        assert_eq!(
            source.render("Alice").expect("renders"),
            "Repository map for Alice"
        );
    }
}
