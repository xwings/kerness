//! JSON values across the language boundary.
//!
//! Dictionary order survives the trip in both directions: `serde_json::Map` is
//! order-preserving here, so a payload built from a Python dict reaches the
//! wire with the keys the caller wrote, in the order they wrote them.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde_json::{Map, Number, Value};

use kerness::conversation::ChatMessage;

/// Render a JSON value as the Python object it stands for.
pub fn value_to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match value {
        Value::Null => py.None().into_bound(py),
        Value::Bool(flag) => PyBool::new(py, *flag).to_owned().into_any(),
        Value::Number(number) => number_to_py(py, number)?,
        Value::String(text) => PyString::new(py, text).into_any(),
        Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(value_to_py(py, item)?)?;
            }
            list.into_any()
        }
        Value::Object(map) => map_to_py(py, map)?.into_any(),
    })
}

fn number_to_py<'py>(py: Python<'py>, number: &Number) -> PyResult<Bound<'py, PyAny>> {
    if let Some(int) = number.as_i64() {
        return Ok(int.into_pyobject(py)?.into_any());
    }
    if let Some(int) = number.as_u64() {
        return Ok(int.into_pyobject(py)?.into_any());
    }
    Ok(number
        .as_f64()
        .unwrap_or_default()
        .into_pyobject(py)?
        .into_any())
}

/// Render a JSON object as a `dict`.
pub fn map_to_py<'py>(py: Python<'py>, map: &Map<String, Value>) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (key, value) in map {
        dict.set_item(key, value_to_py(py, value)?)?;
    }
    Ok(dict)
}

/// Read a Python object as the JSON value it stands for.
///
/// `bool` is tested before `int` because Python's `bool` is a subclass of
/// `int`, and a schema that said `true` would otherwise reach the wire as `1`.
pub fn value_from_py(object: &Bound<'_, PyAny>) -> PyResult<Value> {
    if object.is_none() {
        return Ok(Value::Null);
    }
    if let Ok(flag) = object.downcast::<PyBool>() {
        return Ok(Value::Bool(flag.is_true()));
    }
    if let Ok(int) = object.downcast::<PyInt>() {
        return Ok(Value::Number(int.extract::<i64>()?.into()));
    }
    if let Ok(float) = object.downcast::<PyFloat>() {
        // `inf` and `nan` have no JSON spelling, so they arrive as null rather
        // than as a body no endpoint would accept.
        return Ok(Number::from_f64(float.extract::<f64>()?).map_or(Value::Null, Value::Number));
    }
    if let Ok(text) = object.downcast::<PyString>() {
        return Ok(Value::String(text.to_str()?.to_owned()));
    }
    if let Ok(dict) = object.downcast::<PyDict>() {
        return Ok(Value::Object(map_from_py(dict)?));
    }
    if object.downcast::<PyList>().is_ok() || object.downcast::<PyTuple>().is_ok() {
        let mut items = Vec::new();
        for item in object.try_iter()? {
            items.push(value_from_py(&item?)?);
        }
        return Ok(Value::Array(items));
    }
    Err(PyTypeError::new_err(format!(
        "Object of type {} is not JSON serializable",
        object.get_type().name()?
    )))
}

/// Read a `dict` as a JSON object. Non-string keys are taken as their `str()`.
pub fn map_from_py(dict: &Bound<'_, PyDict>) -> PyResult<Map<String, Value>> {
    let mut map = Map::new();
    for (key, value) in dict.iter() {
        let key = key.str()?.to_string_lossy().into_owned();
        map.insert(key, value_from_py(&value)?);
    }
    Ok(map)
}

/// Read an optional mapping argument, treating `None` as an empty object.
pub fn optional_map(object: Option<&Bound<'_, PyAny>>) -> PyResult<Map<String, Value>> {
    match object {
        None => Ok(Map::new()),
        Some(object) if object.is_none() => Ok(Map::new()),
        Some(object) => map_from_py(object.downcast::<PyDict>()?),
    }
}

/// A chat as the list of `{"role": ..., "content": ...}` dicts a `Provider`
/// receives.
///
/// This is the shape the Python side of the boundary agrees on, so it is
/// written once: a conversation rendered for a turn and a summary request are
/// the same thing to a provider, and two spellings of it would eventually
/// disagree on a key name.
pub fn chat_to_py<'py>(py: Python<'py>, chat: &[ChatMessage]) -> PyResult<Bound<'py, PyList>> {
    let list = PyList::empty(py);
    for message in chat {
        let dict = PyDict::new(py);
        dict.set_item("role", &message.role)?;
        dict.set_item("content", &message.content)?;
        list.append(dict)?;
    }
    Ok(list)
}
