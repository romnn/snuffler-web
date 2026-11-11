#!/usr/bin/env python3
"""buf plugin: generate the typed bridge plumbing from the `BridgeService` in the .proto.

It writes two files:

- `bridge_dispatch.py`: a typed `BridgeServicer` Protocol and `dispatch()`.
  `dispatch()` parses each request into its typed message, calls the typed
  servicer method (proto in, proto out), packs the matching `BridgeResult` oneof
  variant, and wraps everything in the error-as-proto failsafe.
  The variant is read from the BridgeResult descriptor, not hardcoded.
  It imports no grpc runtime (grpcio is not in Pyodide), so it loads in the worker.
  `snuffler_bridge.service` implements the `BridgeServicer` methods; there is no
  hand-written dispatch table.
- `bridge_methods.ts`: the rpc ids as a closed TypeScript union, with each rpc's
  activity lane.
"""

import sys
from typing import Any

from google.protobuf import descriptor_pb2, descriptor_pool, message_factory
from google.protobuf.compiler import plugin_pb2

# Responses paired with the out-of-band zero-copy float32 sample buffer (the same
# convention the Rust ServiceGenerator uses): the handler returns (message, ys).
SAMPLE_BUFFER_RESPONSES = {"ViewMeta"}

# The activity fields that must not be left UNSPECIFIED on any rpc.
REQUIRED_ACTIVITY_FIELDS = ("label", "lane", "policy", "session_access", "cancellation")


def camel_to_snake(name: str) -> str:
    snake = []
    for index, char in enumerate(name):
        if char.isupper() and index > 0:
            snake.append("_")
        snake.append(char.lower())
    return "".join(snake)


def lower_camel(name: str) -> str:
    # protoc-gen-es localName for our PascalCase rpc names: lowercase the first
    # character (MakeExample -> makeExample, GfStoreLabel -> gfStoreLabel).
    return name[:1].lower() + name[1:]


def activity_options(request: plugin_pb2.CodeGeneratorRequest) -> tuple[object, object]:
    """The MethodOptions class and the `snuffler.bridge.v1.activity` extension.

    Both come from a pool built from the request's own files, so the extension
    resolves without importing the generated activity module.
    """
    pool = descriptor_pool.DescriptorPool()
    pool.AddSerializedFile(descriptor_pb2.DESCRIPTOR.serialized_pb)
    for proto in request.proto_file:
        if proto.name == "google/protobuf/descriptor.proto":
            continue
        pool.AddSerializedFile(proto.SerializeToString())
    options_descriptor = pool.FindMessageTypeByName("google.protobuf.MethodOptions")
    return message_factory.GetMessageClass(options_descriptor), pool.FindExtensionByName(
        "snuffler.bridge.v1.activity"
    )


def enum_name(message: object, field: str) -> str:
    descriptor = message.DESCRIPTOR.fields_by_name[field].enum_type
    return descriptor.values_by_number[getattr(message, field)].name


def bridge_methods(
    request: plugin_pb2.CodeGeneratorRequest,
) -> tuple[str, list[dict[str, Any]], set[str]] | None:
    """Every BridgeService rpc, described for both generators.

    Returns the proto package, one dict per rpc, and the `_pb2` modules the
    Python dispatch must import; None when the request holds no BridgeService.
    Exits with an error for an rpc without a complete activity option, or whose
    response has no BridgeResult variant.
    """
    # Fully qualified message name -> its `_pb2` module and class name.
    message_module: dict[str, str] = {}
    message_class: dict[str, str] = {}
    service = None
    service_file = None
    bridge_result = None
    for proto_file in request.proto_file:
        stem = proto_file.name.rsplit("/", 1)[-1].removesuffix(".proto")
        module = stem + "_pb2"
        prefix = ("." + proto_file.package) if proto_file.package else ""
        for message in proto_file.message_type:
            full_name = f"{prefix}.{message.name}"
            message_module[full_name] = module
            message_class[full_name] = message.name
            if message.name == "BridgeResult":
                bridge_result = message
        for candidate in proto_file.service:
            if candidate.name == "BridgeService":
                service = candidate
                service_file = proto_file

    if service is None or bridge_result is None or service_file is None:
        return None

    package = service_file.package
    options_class, activity_extension = activity_options(request)

    # Response message class -> BridgeResult oneof field, read from the descriptor.
    result_variant: dict[str, str] = {}
    for field in bridge_result.field:
        if field.type_name and field.type_name in message_class:
            result_variant[message_class[field.type_name]] = field.name

    methods = []
    used_modules = {"bridge_pb2"}  # BridgeResult / BridgeError / Empty live here
    for method in service.method:
        # Activity option
        options = options_class.FromString(method.options.SerializeToString())
        if not options.HasExtension(activity_extension):
            raise SystemExit(f"BridgeService.{method.name}: missing required activity option")
        activity = options.Extensions[activity_extension]
        empty_fields = [field for field in REQUIRED_ACTIVITY_FIELDS if not getattr(activity, field)]
        if empty_fields:
            raise SystemExit(
                f"BridgeService.{method.name}: activity has empty/UNSPECIFIED "
                + ", ".join(empty_fields)
            )

        # Request and response types
        request_module = message_module[method.input_type]
        response_module = message_module[method.output_type]
        response_class = message_class[method.output_type]
        variant = result_variant.get(response_class)
        if variant is None:
            raise SystemExit(
                f"BridgeService.{method.name}: {response_class} has no BridgeResult variant"
            )
        used_modules.add(request_module)
        used_modules.add(response_module)

        methods.append(
            {
                "python_name": camel_to_snake(method.name),
                # protoc-gen-es localName: the key the TS BridgeMethod map uses.
                "ts_name": lower_camel(method.name),
                # Canonical gRPC method id /package.Service/Method: the same
                # string the Rust client generates and the JS worker relays.
                "method_id": f"/{package}.{service.name}/{method.name}",
                "request_type": f"{request_module}.{message_class[method.input_type]}",
                "response_type": f"{response_module}.{response_class}",
                "result_variant": variant,
                "has_sample_buffer": response_class in SAMPLE_BUFFER_RESPONSES,
                # Only the fields a generated file consumes: Python's lane branch
                # and mutating-session decision, TS's lane assertions.
                # Every field was validated non-UNSPECIFIED above regardless.
                "activity": {
                    "lane": enum_name(activity, "lane").removeprefix("ACTIVITY_LANE_"),
                    "session_access": enum_name(activity, "session_access").removeprefix(
                        "SESSION_ACCESS_"
                    ),
                },
            }
        )
    return package, methods, used_modules


def python_dispatch(package: str, methods: list[dict[str, Any]], used_modules: set[str]) -> str:
    """The source of `bridge_dispatch.py`."""
    out = []

    # Header and imports
    out.append("# @generated by protoc-gen-snuffler-py from BridgeService. DO NOT EDIT.")
    out.append("# A typed servicer Protocol + dispatch for the Pyodide bridge.")
    out.append("# No grpc runtime (grpcio is not in Pyodide), so this imports cleanly in the worker.")
    out.append("# snuffler_bridge.service implements BridgeServicer; there is no hand-written")
    out.append("# dispatch table.")
    out.append("from __future__ import annotations")
    out.append("")
    out.append("import inspect")
    out.append("import traceback")
    out.append(
        "from typing import Any, Awaitable, Dict, Literal, NamedTuple, Protocol, Tuple, Union"
    )
    out.append("")
    out.append(f"from {package} import {', '.join(sorted(used_modules))}")
    out.append("")
    out.append("")

    # Activity table
    lane_values = sorted({method["activity"]["lane"] for method in methods})
    access_values = sorted({method["activity"]["session_access"] for method in methods})
    out.append(f"ActivityLaneName = Literal[{', '.join(map(repr, lane_values))}]")
    out.append(f"SessionAccessName = Literal[{', '.join(map(repr, access_values))}]")
    out.append("")
    out.append("")
    out.append("class ActivityMeta(NamedTuple):")
    out.append("    lane: ActivityLaneName")
    out.append("    session_access: SessionAccessName")
    out.append("")
    out.append("")
    out.append("ACTIVITY_BY_METHOD: Dict[str, ActivityMeta] = {")
    for method in methods:
        activity = method["activity"]
        out.append(
            f'    "{method["method_id"]}": ActivityMeta({activity["lane"]!r}, '
            f'{activity["session_access"]!r}),'
        )
    out.append("}")
    out.append("")
    out.append("")

    # Servicer protocol
    out.append("class BridgeServicer(Protocol):")
    out.append('    """One method per BridgeService rpc: typed proto request in, typed proto')
    out.append("    response out. A handler may be sync or async (network rpcs fetch via")
    out.append("    pyfetch), so each return type is `T | Awaitable[T]`; dispatch awaits when")
    out.append("    needed. `data` is optional out-of-band input (a mounted GF-store path), or")
    out.append("    None.")
    out.append('    """')
    out.append("")
    for method in methods:
        response = method["response_type"]
        base = f"Tuple[{response}, Any]" if method["has_sample_buffer"] else response
        returns = f"Union[{base}, Awaitable[{base}]]"
        out.append(
            f"    def {method['python_name']}(self, request: {method['request_type']}, "
            f"data: Any) -> {returns}: ..."
        )
    out.append("")
    out.append("")

    # Result envelopes
    out.append("def cancelled_result() -> Dict[str, Any]:")
    out.append("    result = bridge_pb2.BridgeResult()")
    out.append("    result.cancelled.SetInParent()")
    out.append('    return {"result": result.SerializeToString()}')
    out.append("")
    out.append("")
    out.append("def failure_result(exc: BaseException) -> Dict[str, Any]:")
    out.append("    result = bridge_pb2.BridgeResult()")
    out.append("    result.error.kind = type(exc).__name__")
    out.append("    result.error.message = str(exc)")
    out.append("    result.error.traceback = traceback.format_exc()")
    out.append('    return {"result": result.SerializeToString()}')
    out.append("")
    out.append("")

    # Dispatch
    out.append("async def dispatch(")
    out.append("    servicer: BridgeServicer, method: str, request: bytes, data: Any")
    out.append(") -> Dict[str, Any]:")
    out.append('    """Route a wire method name to its typed servicer handler, await it when')
    out.append("    the rpc did async work (a network fetch), pack the BridgeResult envelope,")
    out.append("    and turn ANY exception into a BridgeError — never raised across the worker")
    out.append("    boundary (see AGENTS.md).")
    out.append('    """')
    out.append("    result = bridge_pb2.BridgeResult()")
    out.append("    try:")
    for index, method in enumerate(methods):
        keyword = "if" if index == 0 else "elif"
        name = method["python_name"]
        parse = f"{method['request_type']}.FromString(request)"
        # Distinct variable names per method: a shared name would make mypy unify
        # the (different) message types across the if/elif branches and reject it.
        reply_var = f"{name}_out"
        out.append(f'        {keyword} method == "{method["method_id"]}":')
        out.append(f"            {reply_var} = servicer.{name}({parse}, data)")
        out.append(f"            if inspect.isawaitable({reply_var}):")
        out.append(f"                {reply_var} = await {reply_var}")
        if method["has_sample_buffer"]:
            message_var, samples_var = f"{name}_msg", f"{name}_ys"
            out.append(f"            {message_var}, {samples_var} = {reply_var}")
            out.append(f"            result.{method['result_variant']}.CopyFrom({message_var})")
            out.append(
                f'            return {{"result": result.SerializeToString(), "buffer": {samples_var}}}'
            )
        else:
            out.append(f"            result.{method['result_variant']}.CopyFrom({reply_var})")
            out.append('            return {"result": result.SerializeToString()}')
    out.append('        raise KeyError("unknown bridge method: " + method)')
    out.append("    except KeyboardInterrupt:")
    out.append("        return cancelled_result()")
    out.append("    except Exception as exc:  # noqa: BLE001 — the whole point is to catch all")
    out.append("        return failure_result(exc)")
    out.append("")
    return "\n".join(out)


def typescript_methods(methods: list[dict[str, Any]]) -> str:
    """The source of `bridge_methods.ts`.

    protoc-gen-es types the descriptor's typeName/name as plain `string`, so the
    literal ids cannot be recovered from its output.
    They are generated here instead, from the same source the Rust client and
    the Python dispatch use.
    """
    ts = [
        "// @generated by pybuild/snuffler_py_plugin.py from BridgeService. DO NOT EDIT.",
        "// Canonical gRPC method ids (`/package.Service/Method`) for every BridgeService",
        "// rpc, keyed by its protoc-gen-es localName. The literal ids let the TS bridge",
        "// seam type method params as a closed union (BridgeMethodId) instead of `string`,",
        "// so a wrong or mistyped id is a compile error — it cannot drift from the .proto.",
        "",
        "export const BridgeMethod = {",
    ]
    ts += [f'  {method["ts_name"]}: "{method["method_id"]}",' for method in methods]
    ts += [
        "} as const;",
        "",
        "// The localName of a BridgeService rpc (what rpc() accepts).",
        "export type BridgeMethodName = keyof typeof BridgeMethod;",
        "",
        "// The canonical gRPC id of a BridgeService rpc: a closed union of the literal id",
        "// strings above. A plain `string` (or any non-member literal) is NOT assignable.",
        "export type BridgeMethodId = (typeof BridgeMethod)[BridgeMethodName];",
        "",
        "export const BridgeActivity = {",
    ]
    for method in methods:
        lane = method["activity"]["lane"]
        ts.append(f"  [BridgeMethod.{method['ts_name']}]: {{ lane: {lane!r} }},")
    ts += [
        "} as const;",
        "",
        "// Only the field the worker consumes (its per-lane busy assertions); the",
        "// full activity declaration lives in the .proto and the Rust ledger.",
        "export type ActivityMeta = (typeof BridgeActivity)[BridgeMethodId];",
        "",
    ]
    return "\n".join(ts)


def main() -> None:
    request = plugin_pb2.CodeGeneratorRequest.FromString(sys.stdin.buffer.read())
    response = plugin_pb2.CodeGeneratorResponse()
    response.supported_features = plugin_pb2.CodeGeneratorResponse.FEATURE_PROTO3_OPTIONAL

    described = bridge_methods(request)
    if described is not None:
        package, methods, used_modules = described
        package_path = package.replace(".", "/")

        dispatch_file = response.file.add()
        dispatch_file.name = f"{package_path}/bridge_dispatch.py"
        dispatch_file.content = python_dispatch(package, methods, used_modules)

        # The rpc ids as a closed TS union, so the TypeScript bridge seam can type
        # method params as BridgeMethodId instead of `string`.
        methods_file = response.file.add()
        methods_file.name = f"{package_path}/bridge_methods.ts"
        methods_file.content = typescript_methods(methods)

    sys.stdout.buffer.write(response.SerializeToString())


if __name__ == "__main__":
    main()
