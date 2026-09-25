"""Guarded CPython 3.11 startup JSON adapter with public-stdlib fallback."""
import sys

_LIMIT = 4096
_ASCII_WS = " \t\n\r"
_SENTINEL = object()

def _stdlib():
    import json
    # Once its imports are paid, use the public cached functions directly.
    # Preserve an explicit caller/test replacement of any module function.
    namespace = globals()
    for name, original in _ORIGINALS.items():
        if namespace.get(name) is original:
            namespace[name] = getattr(json, name)
    return json

def __getattr__(name):
    if name == "JSONDecodeError":
        return _stdlib().JSONDecodeError
    raise AttributeError(name)

def _constant(value):
    return {"NaN": float("nan"), "Infinity": float("inf"),
            "-Infinity": -float("inf")}[value]

def _skip_ws(text, index):
    while index < len(text) and text[index] in _ASCII_WS:
        index += 1
    return index

def _eligible(text, kwargs):
    return ("json" not in sys.modules and "re" not in sys.modules and sys.implementation.name == "cpython"
            and sys.version_info[:2] == (3, 11) and type(text) is str
            and len(text) <= _LIMIT and not kwargs
            and "NaN" not in text and "Infinity" not in text)

def _scanner(_json, *, object_hook, object_pairs_hook, parse_float, parse_int, parse_constant):
    class Context:
        strict = True
    context = Context()
    context.object_hook = object_hook
    context.object_pairs_hook = object_pairs_hook
    context.parse_float = parse_float
    context.parse_int = parse_int
    context.parse_constant = parse_constant
    return _json.make_scanner(context)

def loads(text, *, object_hook=None, object_pairs_hook=None,
          parse_float=None, parse_int=None, parse_constant=None, **kwargs):
    if not _eligible(text, kwargs):
        return _stdlib().loads(text, object_hook=object_hook, object_pairs_hook=object_pairs_hook,
                               parse_float=parse_float, parse_int=parse_int,
                               parse_constant=parse_constant, **kwargs)
    try:
        import _json
        plain = _scanner(_json, object_hook=None, object_pairs_hook=None,
                         parse_float=float, parse_int=int, parse_constant=_constant)
        _, end = plain(text, _skip_ws(text, 0))
        if _skip_ws(text, end) != len(text):
            raise ValueError("extra JSON data")
    except (ImportError, ValueError, StopIteration, AttributeError, TypeError, SystemError, RecursionError):
        # The probe has no user callbacks. Public json owns malformed errors.
        return _stdlib().loads(text, object_hook=object_hook, object_pairs_hook=object_pairs_hook,
                               parse_float=parse_float, parse_int=parse_int,
                               parse_constant=parse_constant, **kwargs)
    actual = _scanner(_json, object_hook=object_hook, object_pairs_hook=object_pairs_hook,
                      parse_float=parse_float or float, parse_int=parse_int or int,
                      parse_constant=parse_constant or _constant)
    # Never retry this pass: hooks/constants may have side effects.
    result, end = actual(text, _skip_ws(text, 0))
    if _skip_ws(text, end) != len(text):
        raise ValueError("extra JSON data")
    return result

def _default(value):
    raise TypeError(f"Object of type {value.__class__.__name__} is not JSON serializable")

def load(fp, **kwargs):
    return loads(fp.read(), **kwargs)

def dump(value, fp, **kwargs):
    return _stdlib().dump(value, fp, **kwargs)

def dumps(value, *, skipkeys=False, ensure_ascii=True, check_circular=True,
          allow_nan=True, cls=None, indent=None, separators=None, default=None,
          sort_keys=False, **kwargs):
    supported = ("json" not in sys.modules and "re" not in sys.modules and sys.implementation.name == "cpython"
                 and sys.version_info[:2] == (3, 11) and not kwargs and cls is None
                 and indent is None and type(ensure_ascii) is bool and ensure_ascii
                 and type(skipkeys) is bool
                 and type(check_circular) is bool and type(allow_nan) is bool
                 and type(sort_keys) is bool and (default is None or callable(default)))
    if not supported:
        return _stdlib().dumps(value, skipkeys=skipkeys, ensure_ascii=ensure_ascii,
                               check_circular=check_circular, allow_nan=allow_nan, cls=cls,
                               indent=indent, separators=separators, default=default,
                               sort_keys=sort_keys, **kwargs)
    if separators is None:
        item_separator, key_separator = ", ", ": "
    elif type(separators) is tuple and len(separators) == 2 and all(type(x) is str for x in separators):
        item_separator, key_separator = separators
    else:
        return _stdlib().dumps(value, skipkeys=skipkeys, ensure_ascii=ensure_ascii,
                               check_circular=check_circular, allow_nan=allow_nan, cls=cls,
                               indent=indent, separators=separators, default=default,
                               sort_keys=sort_keys)
    try:
        import _json
        encoder = _json.make_encoder({} if check_circular else None, _default if default is None else default,
                                     _json.encode_basestring_ascii, None, key_separator,
                                     item_separator, sort_keys, skipkeys, allow_nan)
    except (ImportError, AttributeError, TypeError):
        return _stdlib().dumps(value, skipkeys=skipkeys, ensure_ascii=ensure_ascii,
                               check_circular=check_circular, allow_nan=allow_nan, cls=cls,
                               indent=indent, separators=separators, default=default,
                               sort_keys=sort_keys)
    return "".join(encoder(value, 0))

_ORIGINALS = {name: globals()[name] for name in ("loads", "dumps", "load", "dump")}
