"""Tiny strict-job entry point for timed Python SQLLogic proxy invocations."""
import sys


def main(argv=None):
    arguments = sys.argv[1:] if argv is None else argv
    if len(arguments) != 1 or not arguments[0].startswith("--once-job="):
        raise SystemExit("sqllogic_proxy_once requires exactly one --once-job=<JSON> argument")
    from proxy_once_core import bootstrap
    return bootstrap(arguments[0].split("=", 1)[1])


if __name__ == "__main__":
    raise SystemExit(main())
