"""Shared scalar validation for sender setup and encoding."""


def _integer(value: int, name: str, low: int, high: int) -> None:
    if type(value) is not int:
        raise TypeError(f"{name} must be an integer")
    if not low <= value <= high:
        raise ValueError(f"{name} must be between {low} and {high}")
