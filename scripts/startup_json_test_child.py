"""Minimal fresh interpreter helper for test_startup_json.py."""
from __future__ import annotations

import ast
import math
import os
import sys


def main(encoded_spec: str) -> None:
    operation, payload, options, _expectation = ast.literal_eval(encoded_spec)
    if "json" in sys.modules:
        raise RuntimeError("child startup imported json before candidate invocation")
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import startup_json as candidate

    class HookFailure(ValueError):
        pass

    class FalseyDefault:
        def __bool__(self): return False
        def __call__(self, value): raise HookFailure("falsey default called")

    class SpoofedClass: pass
    class ClassSpoof:
        @property
        def __class__(self): return SpoofedClass

    class ReadCounter:
        def __init__(self, text): self.text, self.reads = text, 0
        def read(self): self.reads += 1; return self.text

    def invoke(module, events):
        kwargs = dict(options)
        already = kwargs.pop("already_imported", False)
        if already:
            import json  # noqa: F401
        if kwargs.get("object_hook") == "object":
            kwargs["object_hook"] = lambda value: events.append(("object", sorted(value))) or value
        if kwargs.get("object_pairs_hook") == "pairs":
            kwargs["object_pairs_hook"] = lambda pairs: events.append(("pairs", pairs)) or dict(pairs)
        if kwargs.get("object_pairs_hook") == "pairs_reject":
            kwargs["object_pairs_hook"] = lambda pairs: events.append(("pairs", pairs)) or (_ for _ in ()).throw(HookFailure("pairs rejected"))
        if kwargs.get("parse_int") == "int":
            kwargs["parse_int"] = lambda value: events.append(("int", value)) or int(value)
        if kwargs.get("parse_float") == "float":
            kwargs["parse_float"] = lambda value: events.append(("float", value)) or float(value)
        if kwargs.get("parse_constant") == "constant":
            kwargs["parse_constant"] = lambda value: events.append(("constant", value)) or value
        if kwargs.get("parse_constant") == "constant_reject":
            kwargs["parse_constant"] = lambda value: events.append(("constant", value)) or (_ for _ in ()).throw(HookFailure("constant rejected"))
        if kwargs.get("default") == "default":
            kwargs["default"] = lambda value: events.append(("default", type(value).__name__)) or "default-value"
        if kwargs.get("default") == "default_reject":
            kwargs["default"] = lambda value: events.append(("default", type(value).__name__)) or (_ for _ in ()).throw(HookFailure("default rejected"))
        if kwargs.get("default") == "falsey": kwargs["default"] = FalseyDefault()
        if operation in ("loads", "load"):
            if operation == "load":
                file = ReadCounter(payload)
                result = module.load(file, **kwargs)
                events.append(("read_count", file.reads))
                return result
            result = module.loads(payload, **kwargs)
            if payload == '[NaN,NaN]':
                events.append(("repeated_nan_identity", result[0] is result[1]))
            return result
        if kwargs.get("cls") == "bad": kwargs["cls"] = type("BadEncoder", (), {})
        values = {
            "nested": {"a": [1, True, None], "b": {"c": "x"}}, "unicode": {"snowman": "☃"},
            "unordered": {"z": 1, "a": 2}, "nan": float("nan"), "unknown": object(),
            "skipkeys": {1: "one", "ok": 2}, "spoofed_class": ClassSpoof(),
        }
        if payload == "cycle":
            value = []; value.append(value)
        else:
            value = values[payload]
        return module.dumps(value, **kwargs)

    def normalize(value):
        if value is None: return ("none",)
        if type(value) is bool: return ("bool", value)
        if type(value) is int: return ("int", str(value))
        if type(value) is float:
            if math.isnan(value): return ("float", "nan")
            if math.isinf(value): return ("float", "inf" if value > 0 else "-inf")
            return ("float", value.hex())
        if type(value) is str: return ("str", value.encode("unicode_escape").decode("ascii"))
        if type(value) is bytes: return ("bytes", value.hex())
        if type(value) in (list, tuple): return (type(value).__name__, [normalize(item) for item in value])
        if type(value) is dict: return ("dict", [(normalize(k), normalize(v)) for k, v in value.items()])
        return ("other", type(value).__module__, type(value).__qualname__)

    def snapshot(module):
        events = []
        try:
            value = invoke(module, events)
            return ("ok", normalize(value), normalize(events))
        except BaseException as error:
            result = ("error", type(error).__module__, type(error).__qualname__, str(error), normalize(events))
            if all(hasattr(error, key) for key in ("pos", "lineno", "colno", "doc")):
                result += (error.pos, error.lineno, error.colno, error.doc.encode("unicode_escape").decode("ascii"))
            return result

    candidate_result = snapshot(candidate)
    candidate_kept_json_unloaded = "json" not in sys.modules
    import json
    print(repr((candidate_result, snapshot(json), candidate_kept_json_unloaded)))


if __name__ == "__main__":
    if len(sys.argv) != 2: raise SystemExit("expected one literal case specification")
    main(sys.argv[1])
