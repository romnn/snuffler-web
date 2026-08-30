#!/usr/bin/env python3
"""buf plugin: generate a typed `BridgeServicer` Protocol + `dispatch()` for the
Pyodide bridge, from the `BridgeService` defined in the .proto.

The output imports no grpc runtime (grpcio is not in Pyodide), so it loads in the
worker. `dispatch()` parses each request into its typed message, calls the typed
servicer method (proto in, proto out), packs the matching `BridgeResult` oneof
variant — the variant is read from the BridgeResult descriptor, not hardcoded —
and wraps everything in the error-as-proto failsafe. bridge.py implements the
`BridgeServicer` methods; there is no hand-written dispatch table any more.
"""

import sys

from google.protobuf import descriptor_pb2, descriptor_pool, message_factory
from google.protobuf.compiler import plugin_pb2

# Responses paired with the out-of-band zero-copy float32 buffer (same convention
# the Rust ServiceGenerator uses): the handler returns (message, ys).
BUFFER_RESPONSES = {"ViewMeta", "SpecMeta"}


def camel_to_snake(name: str) -> str:
    out = []
    for i, ch in enumerate(name):
        if ch.isupper() and i > 0:
            out.append("_")
        out.append(ch.lower())
    return "".join(out)


def lower_camel(name: str) -> str:
    # protoc-gen-es localName for our PascalCase rpc names: lowercase the first
    # character (MakeExample -> makeExample, GfStoreLabel -> gfStoreLabel).
    return name[:1].lower() + name[1:]


def activity_options(req: plugin_pb2.CodeGeneratorRequest) -> tuple[object, object]:
    pool = descriptor_pool.DescriptorPool()
    pool.AddSerializedFile(descriptor_pb2.DESCRIPTOR.serialized_pb)
    for proto in req.proto_file:
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


def main() -> None:
    req = plugin_pb2.CodeGeneratorRequest.FromString(sys.stdin.buffer.read())
    resp = plugin_pb2.CodeGeneratorResponse()
    resp.supported_features = plugin_pb2.CodeGeneratorResponse.FEATURE_PROTO3_OPTIONAL

    # Fully-qualified message name -> (module alias, class name).
    msg_module: dict[str, str] = {}
    msg_class: dict[str, str] = {}
    service = None
    service_file = None
    bridge_result = None
    for f in req.proto_file:
        stem = f.name.rsplit("/", 1)[-1]
        if stem.endswith(".proto"):
            stem = stem[:-6]
        alias = stem + "_pb2"
        prefix = ("." + f.package) if f.package else ""
        for m in f.message_type:
            fq = f"{prefix}.{m.name}"
            msg_module[fq] = alias
            msg_class[fq] = m.name
            if m.name == "BridgeResult":
                bridge_result = m
        for s in f.service:
            if s.name == "BridgeService":
                service = s
                service_file = f

    if service is None or bridge_result is None or service_file is None:
        sys.stdout.buffer.write(resp.SerializeToString())
        return

    pkg = service_file.package
    pkg_path = pkg.replace(".", "/")
    options_class, activity_extension = activity_options(req)

    # Response message class -> BridgeResult oneof field, read from the descriptor.
    variant_field: dict[str, str] = {}
    for fld in bridge_result.field:
        if fld.type_name and fld.type_name in msg_class:
            variant_field[msg_class[fld.type_name]] = fld.name

    methods = []
    used_modules = {"bridge_pb2"}  # BridgeResult / BridgeError / Empty live here
    for m in service.method:
        options = options_class.FromString(m.options.SerializeToString())
        if not options.HasExtension(activity_extension):
            raise SystemExit(f"BridgeService.{m.name}: missing required activity option")
        activity = options.Extensions[activity_extension]
        empty_fields = [
            field
            for field in ("label", "lane", "policy", "effect", "cancel")
            if not getattr(activity, field)
        ]
        if empty_fields:
            raise SystemExit(
                f"BridgeService.{m.name}: activity has empty/UNSPECIFIED "
                + ", ".join(empty_fields)
            )
        req_mod, req_cls = msg_module[m.input_type], msg_class[m.input_type]
        resp_mod, resp_cls = msg_module[m.output_type], msg_class[m.output_type]
        variant = variant_field.get(resp_cls)
        if variant is None:
            raise SystemExit(f"BridgeService.{m.name}: {resp_cls} has no BridgeResult variant")
        used_modules.add(req_mod)
        used_modules.add(resp_mod)
        methods.append(
            {
                "snake": camel_to_snake(m.name),
                # protoc-gen-es localName — the key the TS BridgeMethod map uses.
                "local": lower_camel(m.name),
                # Canonical gRPC method id /package.Service/Method — the same
                # string the Rust client generates and the JS worker relays.
                "wire": f"/{pkg}.{service.name}/{m.name}",
                "req": f"{req_mod}.{req_cls}",
                "resp": f"{resp_mod}.{resp_cls}",
                "variant": variant,
                "buffer": resp_cls in BUFFER_RESPONSES,
                "activity": {
                    "label": activity.label,
                    "lane": enum_name(activity, "lane").removeprefix("ACTIVITY_LANE_"),
                    "policy": enum_name(activity, "policy").removeprefix("ACTIVITY_POLICY_"),
                    "effect": enum_name(activity, "effect").removeprefix("ACTIVITY_EFFECT_"),
                    "background": activity.background,
                    "cancel": enum_name(activity, "cancel").removeprefix("ACTIVITY_CANCEL_"),
                    "supersedes": activity.supersedes,
                    "marker_output": activity.marker_output,
                },
            }
        )

    out = []
    out.append("# @generated by protoc-gen-snuffler-py from BridgeService. DO NOT EDIT.")
    out.append("# A typed servicer Protocol + dispatch for the Pyodide bridge. No grpc runtime")
    out.append("# (grpcio is not in Pyodide), so this imports cleanly in the worker. bridge.py")
    out.append("# implements BridgeServicer; there is no hand-written dispatch table.")
    out.append("from __future__ import annotations")
    out.append("")
    out.append("import inspect")
    out.append("import traceback")
    out.append("from typing import Any, Awaitable, Dict, NamedTuple, Protocol, Tuple, Union")
    out.append("")
    out.append(f"from {pkg} import {', '.join(sorted(used_modules))}")
    out.append("")
    out.append("")
    out.append("class ActivityMeta(NamedTuple):")
    out.append("    label: str")
    out.append("    lane: str")
    out.append("    policy: str")
    out.append("    effect: str")
    out.append("    background: bool")
    out.append("    cancel: str")
    out.append("    supersedes: bool")
    out.append("    marker_output: bool")
    out.append("")
    out.append("")
    out.append("ACTIVITY_BY_METHOD: Dict[str, ActivityMeta] = {")
    for mm in methods:
        activity = mm["activity"]
        out.append(
            f'    "{mm["wire"]}": ActivityMeta('
            f'{activity["label"]!r}, {activity["lane"]!r}, {activity["policy"]!r}, '
            f'{activity["effect"]!r}, {activity["background"]!r}, {activity["cancel"]!r}, '
            f'{activity["supersedes"]!r}, {activity["marker_output"]!r}),'
        )
    out.append("}")
    out.append("")
    out.append("")
    out.append("class BridgeServicer(Protocol):")
    out.append('    """One method per BridgeService rpc: typed proto request in, typed proto')
    out.append("    response out. A handler may be sync or async (network rpcs fetch via")
    out.append("    pyfetch), so each return type is `T | Awaitable[T]`; dispatch awaits when")
    out.append("    needed. `data` is optional out-of-band input (a mounted GF-store path), or")
    out.append("    None.")
    out.append('    """')
    out.append("")
    for mm in methods:
        base = f"Tuple[{mm['resp']}, Any]" if mm["buffer"] else mm["resp"]
        ret = f"Union[{base}, Awaitable[{base}]]"
        out.append(f"    def {mm['snake']}(self, request: {mm['req']}, data: Any) -> {ret}: ...")
    out.append("")
    out.append("")
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
    for i, mm in enumerate(methods):
        kw = "if" if i == 0 else "elif"
        parse = f"{mm['req']}.FromString(request)"
        # Distinct `<snake>_out` per method: a shared name would make mypy unify
        # the (different) message types across the if/elif branches and reject it.
        out_var = f"{mm['snake']}_out"
        out.append(f'        {kw} method == "{mm["wire"]}":')
        out.append(f"            {out_var} = servicer.{mm['snake']}({parse}, data)")
        out.append(f"            if inspect.isawaitable({out_var}):")
        out.append(f"                {out_var} = await {out_var}")
        if mm["buffer"]:
            # Distinct names per buffer method: a shared one would make mypy unify
            # the (different) message types across the if/elif branches.
            msg_var, ys_var = f"{mm['snake']}_msg", f"{mm['snake']}_ys"
            out.append(f"            {msg_var}, {ys_var} = {out_var}")
            out.append(f"            result.{mm['variant']}.CopyFrom({msg_var})")
            out.append(
                f'            return {{"result": result.SerializeToString(), "buffer": {ys_var}}}'
            )
        else:
            out.append(f"            result.{mm['variant']}.CopyFrom({out_var})")
            out.append('            return {"result": result.SerializeToString()}')
    out.append('        raise KeyError("unknown bridge method: " + method)')
    out.append("    except KeyboardInterrupt:")
    out.append("        cancelled = bridge_pb2.BridgeResult()")
    out.append("        cancelled.cancelled.SetInParent()")
    out.append('        return {"result": cancelled.SerializeToString()}')
    out.append("    except Exception as exc:  # noqa: BLE001 — the whole point is to catch all")
    out.append("        err = bridge_pb2.BridgeResult()")
    out.append("        err.error.kind = type(exc).__name__")
    out.append("        err.error.message = str(exc)")
    out.append("        err.error.traceback = traceback.format_exc()")
    out.append('        return {"result": err.SerializeToString()}')
    out.append("")

    f = resp.file.add()
    f.name = f"{pkg_path}/bridge_dispatch.py"
    f.content = "\n".join(out)

    # Second output: the canonical gRPC method ids as a closed TS union, so the
    # TypeScript bridge seam can type method params as BridgeMethodId instead of
    # `string`. protoc-gen-es types the descriptor's typeName/name as plain
    # `string`, so the literal ids can't be recovered from it — they must be
    # generated here (the same source the Rust client and Python dispatch use).
    ts = [
        "// @generated by pybuild/snuffler_py_plugin.py from BridgeService. DO NOT EDIT.",
        "// Canonical gRPC method ids (`/package.Service/Method`) for every BridgeService",
        "// rpc, keyed by its protoc-gen-es localName. The literal ids let the TS bridge",
        "// seam type method params as a closed union (BridgeMethodId) instead of `string`,",
        "// so a wrong or mistyped id is a compile error — it cannot drift from the .proto.",
        "",
        "export const BridgeMethod = {",
    ]
    ts += [f'  {mm["local"]}: "{mm["wire"]}",' for mm in methods]
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
    for mm in methods:
        activity = mm["activity"]
        ts.append(f"  [BridgeMethod.{mm['local']}]: {{")
        ts.append(f"    label: {activity['label']!r},")
        ts.append(f"    lane: {activity['lane']!r},")
        ts.append(f"    policy: {activity['policy']!r},")
        ts.append(f"    effect: {activity['effect']!r},")
        ts.append(f"    background: {str(activity['background']).lower()},")
        ts.append(f"    cancel: {activity['cancel']!r},")
        ts.append(f"    supersedes: {str(activity['supersedes']).lower()},")
        ts.append(f"    markerOutput: {str(activity['marker_output']).lower()},")
        ts.append("  },")
    ts += [
        "} as const satisfies Record<BridgeMethodId, ActivityMeta>;",
        "",
        "export type ActivityMeta = {",
        "  label: string;",
        '  lane: "JOB" | "VIEW";',
        '  policy: "QUEUE" | "DEDUP" | "LATEST_WINS" | "EXCLUSIVE";',
        '  effect: "READ" | "MUTATE";',
        "  background: boolean;",
        '  cancel: "NOT_CANCELLABLE" | "COOPERATIVE" | "INTERRUPT";',
        "  supersedes: boolean;",
        "  markerOutput: boolean;",
        "};",
        "",
    ]
    tf = resp.file.add()
    tf.name = f"{pkg_path}/bridge_methods.ts"
    tf.content = "\n".join(ts)

    sys.stdout.buffer.write(resp.SerializeToString())


if __name__ == "__main__":
    main()
