"""Compile the standalone packet sender extension with the PDM backend."""

from setuptools_rust import Binding, RustExtension


def pdm_build_update_setup_kwargs(
    context: object, setup_kwargs: dict[str, object]
) -> None:
    setup_kwargs["rust_extensions"] = [
        RustExtension(
            "ledfx_senders._native",
            path="native/Cargo.toml",
            binding=Binding.PyO3,
            debug=False,
            optional=False,
            cargo_manifest_args=["--locked"],
        )
    ]
