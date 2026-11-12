"""Prove that bridge RPCs with missing or unsafe activity metadata are rejected."""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

from google.protobuf import descriptor_pb2
from google.protobuf.compiler import plugin_pb2


def reject_fixture(filename: str, expected: str) -> None:
    root = Path(__file__).resolve().parents[1]
    fixture = root / "tests/fixtures" / filename
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
    stderr = result.stderr.decode(errors="replace")
    if result.returncode == 0 or expected not in stderr:
        raise SystemExit(
            f"{filename} was not rejected as expected: exit={result.returncode}, stderr={stderr!r}"
        )


def main() -> None:
    reject_fixture(
        "missing_activity.proto", "BridgeService.MissingActivity: missing required activity option"
    )
    reject_fixture(
        "latest_wins_mutation.proto",
        "BridgeService.LatestWinsMutation: LATEST_WINS requires SESSION_ACCESS_READ_ONLY",
    )


if __name__ == "__main__":
    main()
