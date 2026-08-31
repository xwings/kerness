//! `kerness._core` — the Rust framework, as a Python extension module.
//!
//! Nothing here decides anything. Every class is a thin wrapper over a type in
//! the `kerness` crate, and every function forwards. What does live here is the
//! translation: JSON values across the boundary, framework errors as exception
//! instances and back, and Python callables seen as the traits the framework
//! calls.
//!
//! A handful of pieces are deliberately Python and are handed down at import
//! rather than defined here — the exception hierarchy, whose two-argument
//! constructors a `create_exception!` cannot express, and the tool dialect,
//! which callers compare with `is` and which therefore has to be a real
//! `enum.Enum` member.

mod access;
mod channel;
mod convert;
mod errors;
mod funcs;
mod memory;
mod provider;
mod runtime;
mod session;
mod skill;
mod types;

use pyo3::prelude::*;

/// Hand the Python-side declarations down, and point the framework at the
/// assets that ship beside them.
///
/// Called once by `kerness/__init__.py`, which is the only place that knows
/// where the package was installed.
#[pyfunction]
fn bootstrap(
    exceptions: &Bound<'_, PyAny>,
    dialect: &Bound<'_, PyAny>,
    assets_root: &str,
) -> PyResult<()> {
    errors::register(exceptions)?;
    types::register_dialect(dialect);
    kerness::assets::set_root(assets_root);
    provider::install_transport();
    channel::install_console_writer();
    channel::install_logger();
    access::install_console();
    Ok(())
}

#[pymodule]
fn _core(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("VERSION", kerness::VERSION)?;
    module.add_function(wrap_pyfunction!(bootstrap, module)?)?;
    module.add_function(wrap_pyfunction!(access::prompt_on_console, module)?)?;
    module.add_function(wrap_pyfunction!(provider::http_post_json, module)?)?;
    module.add_function(wrap_pyfunction!(
        provider::convert_messages_for_claude,
        module
    )?)?;

    module.add_class::<types::PyToolCall>()?;
    module.add_class::<types::PyToolSpec>()?;
    module.add_class::<types::PyToolResult>()?;
    module.add_class::<types::PyProviderResponse>()?;
    module.add_class::<types::PyMessage>()?;
    module.add_class::<types::PyTurn>()?;
    module.add_class::<types::PyAgent>()?;
    module.add_class::<types::PyMemory>()?;
    module.add_class::<memory::PyFileMemory>()?;
    module.add_class::<memory::PySessionMemory>()?;
    module.add_class::<types::PyPersonaConfig>()?;
    module.add_class::<types::PyRoleConfig>()?;
    module.add_class::<types::PySkillConfig>()?;
    module.add_class::<types::PyOrchestratorSpec>()?;
    module.add_class::<types::PyParticipantSpec>()?;
    module.add_class::<types::PyAgentsSpec>()?;
    module.add_class::<types::PyPhaseSpec>()?;
    module.add_class::<types::PyLoopSpec>()?;
    module.add_class::<types::PyResultField>()?;
    module.add_class::<types::PyHarnessSpec>()?;
    module.add_class::<types::PyPermitted>()?;
    module.add_class::<types::PyGameplanConfig>()?;
    module.add_class::<provider::PyProviderCore>()?;
    module.add_class::<channel::PyConsoleChannel>()?;
    module.add_class::<channel::PyFileChannel>()?;
    module.add_class::<channel::PyLogChannel>()?;
    module.add_class::<channel::PyMultiChannel>()?;
    module.add_class::<access::PyAccessRequest>()?;
    module.add_class::<access::PyAccessManager>()?;
    module.add_class::<skill::PySkillActivation>()?;
    module.add_class::<skill::PySkillRegistry>()?;
    module.add_class::<runtime::PyConversation>()?;
    module.add_class::<runtime::PyToolDispatcher>()?;
    module.add_class::<runtime::PyPromptAssembler>()?;
    module.add_class::<runtime::PyAgentRunner>()?;
    module.add_class::<runtime::PyLoopState>()?;
    module.add_class::<runtime::PyOrchestratorLoop>()?;
    module.add_class::<session::PySession>()?;
    module.add_class::<session::PySessionResult>()?;

    funcs::register(module)?;
    Ok(())
}
