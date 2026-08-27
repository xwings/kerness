//! Skills: the files on disk, and what one agent turn does with them.
//!
//! [`loader`] turns a `SKILL.md` into a [`loader::SkillConfig`]; [`runtime`]
//! decides when an agent gets to read one. The split is the point of the
//! feature — a prompt carries every skill's one-line description, and the body
//! only arrives if the agent asks for it.

pub mod loader;
pub mod runtime;
