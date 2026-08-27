"""Focused tests for JSON Schema argument validation."""

from kerness.jsonschema import validate_arguments


def test_closed_empty_object_rejects_every_argument():
    """``additionalProperties: false`` still applies when there are no
    declared properties; an empty mapping is not permission for arbitrary keys.
    """
    schema = {
        "type": "object",
        "properties": {},
        "additionalProperties": False,
    }

    assert validate_arguments(schema, {"surprise": 1}) == [
        "unexpected argument 'surprise'"
    ]
