"""Prove that an unclassified bridge RPC is rejected during code generation."""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

from google.protobuf import descriptor_pb2
from google.protobuf.compiler import plugin_pb2


def main() -> None:
    root = Path(__file__).resolve().parents[1]
    fixture = root / "tests/fixtures/missing_activity.proto"
    with tempfile.TemporaryDirectory(prefix="snuffler-codegen-") as temporary:
        descriptor_path = Path(temporary) / "fixture.bin"
        subprocess.run(
            [
                "protoc",
                f"--proto_path={root}",
                f"--proto_path={root / 'proto'}",
                "--include_imports",
                f"--descriptor_set_out={descriptor_path}",
                str(fixture.relative_to(root)),
            ],
            cwd=root,
            check=True,
        )
        descriptors = descriptor_pb2.FileDescriptorSet.FromString(descriptor_path.read_bytes())

    request = plugin_pb2.CodeGeneratorRequest(
        file_to_generate=[str(fixture.relative_to(root))],
        proto_file=descriptors.file,
    )
    result = subprocess.run(
        [sys.executable, str(root / "pybuild/snuffler_py_plugin.py")],
        cwd=root,
        input=request.SerializeToString(),
        capture_output=True,
        check=False,
    )
    expected = "BridgeService.MissingActivity: missing required activity option"
    stderr = result.stderr.decode(errors="replace")
    if result.returncode == 0 or expected not in stderr:
        raise SystemExit(
            f"missing-activity fixture was not rejected as expected: "
            f"exit={result.returncode}, stderr={stderr!r}"
        )


if __name__ == "__main__":
    main()
