#!/usr/bin/env python3
"""Run a reproducible local forward-rs performance smoke benchmark.

The harness starts a real `forward-server`, a real `forward-client`, and
`iperf3` on loopback. It measures:

1. Direct iperf3 throughput to the target.
2. Throughput through a pushed forward-rs TCP rule.
3. Optional ingress bandwidth-cap convergence through a second pushed rule.

It intentionally uses only the Python standard library plus an installed
`iperf3` binary so operators can copy the workflow to bare VPS hosts.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from dataclasses import dataclass
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parent.parent


@dataclass(frozen=True)
class Ports:
    control: int
    operator_http: int
    metrics: int
    iperf_target: int
    uncapped_listen: int
    capped_listen: int


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_for_metrics(port: int, deadline_seconds: float = 10.0) -> None:
    deadline = time.time() + deadline_seconds
    url = f"http://127.0.0.1:{port}/metrics"
    last_error: Exception | None = None
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=0.25).read()
            return
        except Exception as exc:  # noqa: BLE001 - preserve useful startup context.
            last_error = exc
            time.sleep(0.1)
    raise RuntimeError(f"forward-server did not expose {url}: {last_error}")


def check_output(cmd: list[str], env: dict[str, str]) -> str:
    return subprocess.check_output(cmd, env=env, stderr=subprocess.STDOUT, text=True)


def run_iperf3(iperf3: str, port: int, seconds: int, omit_seconds: int) -> dict[str, Any]:
    output = check_output(
        [
            iperf3,
            "-J",
            "-c",
            "127.0.0.1",
            "-p",
            str(port),
            "-t",
            str(seconds),
            "-O",
            str(omit_seconds),
        ],
        os.environ.copy(),
    )
    data = json.loads(output)
    received = data["end"]["sum_received"]["bits_per_second"]
    sent = data["end"]["sum_sent"]
    return {
        "mbps": round(received / 1_000_000, 3),
        "retransmits": sent.get("retransmits"),
    }


def start_iperf3_server(iperf3: str, port: int) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [iperf3, "-s", "-p", str(port), "-1"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )


def write_config(config_dir: pathlib.Path, ports: Ports, token: str) -> None:
    (config_dir / "server.toml").write_text(
        f"""control_listen = "127.0.0.1:{ports.control}"
operator_http_listen = "127.0.0.1:{ports.operator_http}"
metrics_listen = "127.0.0.1:{ports.metrics}"
tls_cert_path = "{config_dir / 'server.crt'}"
tls_key_path = "{config_dir / 'server.key'}"
token_store_path = "{config_dir / 'tokens.json'}"
operator_store_path = "{config_dir / 'identity.json'}"
operator_token = "{token}"
shutdown_drain_timeout_secs = 30
log_format = "compact"
range_rule_max_ports = 1024
""",
        encoding="utf-8",
    )


def terminate_all(processes: list[subprocess.Popen[Any]]) -> None:
    for proc in reversed(processes):
        if proc.poll() is not None:
            continue
        proc.terminate()
        try:
            proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            proc.kill()


def build_release(server_bin: pathlib.Path, client_bin: pathlib.Path) -> None:
    if server_bin.exists() and client_bin.exists():
        return
    env = os.environ.copy()
    env.setdefault("FORWARD_SKIP_WEBUI", "1")
    subprocess.check_call(
        ["cargo", "build", "--release", "-p", "forward-server", "-p", "forward-client"],
        cwd=ROOT,
        env=env,
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=int, default=5)
    parser.add_argument("--omit-seconds", type=int, default=1)
    parser.add_argument(
        "--cap-bytes-per-sec",
        type=int,
        default=1_048_576,
        help="Set 0 to skip the capped-rule convergence check.",
    )
    parser.add_argument("--json-out", type=pathlib.Path)
    parser.add_argument("--iperf3", default=shutil.which("iperf3") or "iperf3")
    parser.add_argument("--server-bin", type=pathlib.Path, default=ROOT / "target/release/forward-server")
    parser.add_argument("--client-bin", type=pathlib.Path, default=ROOT / "target/release/forward-client")
    args = parser.parse_args()

    build_release(args.server_bin, args.client_bin)
    if not shutil.which(args.iperf3) and not pathlib.Path(args.iperf3).exists():
        raise SystemExit("iperf3 is required. Install it, or pass --iperf3 /path/to/iperf3.")

    ports = Ports(
        control=free_port(),
        operator_http=free_port(),
        metrics=free_port(),
        iperf_target=free_port(),
        uncapped_listen=free_port(),
        capped_listen=free_port(),
    )

    processes: list[subprocess.Popen[Any]] = []
    try:
        with tempfile.TemporaryDirectory(prefix="forward-rs-perf-") as tmp:
            tmpdir = pathlib.Path(tmp)
            config_dir = tmpdir / "config"
            data_dir = tmpdir / "data"
            config_dir.mkdir()
            data_dir.mkdir()

            token = check_output([str(args.server_bin), "gen-token"], os.environ.copy()).strip()
            write_config(config_dir, ports, token)

            env = os.environ.copy()
            env["FORWARD_OPERATOR_TOKEN"] = token
            env.setdefault("RUST_LOG", "warn")

            bundle = tmpdir / "edge-01.bundle.json"
            check_output(
                [
                    str(args.server_bin),
                    "--config-dir",
                    str(config_dir),
                    "--data-dir",
                    str(data_dir),
                    "--advertised-endpoint",
                    f"127.0.0.1:{ports.control}",
                    "provision-client",
                    "edge-01",
                    "--out",
                    str(bundle),
                ],
                env,
            )

            server = subprocess.Popen(
                [
                    str(args.server_bin),
                    "--config-dir",
                    str(config_dir),
                    "--data-dir",
                    str(data_dir),
                    "--advertised-endpoint",
                    f"127.0.0.1:{ports.control}",
                    "serve",
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.STDOUT,
                env=env,
            )
            processes.append(server)
            wait_for_metrics(ports.metrics)

            client = subprocess.Popen(
                [
                    str(args.client_bin),
                    "--bundle",
                    str(bundle),
                    "--stats-report-interval-secs",
                    "1",
                ],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.STDOUT,
                env=env,
            )
            processes.append(client)
            time.sleep(1.5)

            iperf_server = start_iperf3_server(args.iperf3, ports.iperf_target)
            time.sleep(0.3)
            direct = run_iperf3(args.iperf3, ports.iperf_target, args.seconds, args.omit_seconds)
            iperf_server.wait(timeout=args.seconds + 5)

            iperf_server = start_iperf3_server(args.iperf3, ports.iperf_target)
            time.sleep(0.3)
            uncapped_rule = check_output(
                [
                    str(args.server_bin),
                    "--config-dir",
                    str(config_dir),
                    "--data-dir",
                    str(data_dir),
                    "push-rule",
                    "edge-01",
                    str(ports.uncapped_listen),
                    f"127.0.0.1:{ports.iperf_target}",
                    "--http-endpoint",
                    f"127.0.0.1:{ports.operator_http}",
                ],
                env,
            ).strip()
            time.sleep(0.8)
            uncapped = run_iperf3(args.iperf3, ports.uncapped_listen, args.seconds, args.omit_seconds)
            iperf_server.wait(timeout=args.seconds + 5)

            result: dict[str, Any] = {
                "host": check_output(["uname", "-a"], env).strip(),
                "rustc": check_output(["rustc", "--version"], env).strip(),
                "iperf3": check_output([args.iperf3, "--version"], env).splitlines()[0],
                "duration_seconds": args.seconds,
                "omit_seconds": args.omit_seconds,
                "direct": direct,
                "forward_rs_uncapped": uncapped,
                "forward_vs_direct_pct": round(uncapped["mbps"] / direct["mbps"] * 100.0, 2),
                "uncapped_rule_id": uncapped_rule,
            }

            if args.cap_bytes_per_sec > 0:
                iperf_server = start_iperf3_server(args.iperf3, ports.iperf_target)
                time.sleep(0.3)
                capped_rule = check_output(
                    [
                        str(args.server_bin),
                        "--config-dir",
                        str(config_dir),
                        "--data-dir",
                        str(data_dir),
                        "push-rule",
                        "edge-01",
                        str(ports.capped_listen),
                        "--target",
                        f"127.0.0.1:{ports.iperf_target}",
                        "--bandwidth-in-bps",
                        str(args.cap_bytes_per_sec),
                        "--http-endpoint",
                        f"127.0.0.1:{ports.operator_http}",
                    ],
                    env,
                ).strip()
                time.sleep(0.8)
                capped = run_iperf3(args.iperf3, ports.capped_listen, max(args.seconds, 8), max(args.omit_seconds, 2))
                iperf_server.wait(timeout=max(args.seconds, 8) + 5)
                target_mbps = args.cap_bytes_per_sec * 8 / 1_000_000
                result["forward_rs_capped"] = {
                    **capped,
                    "rule_id": capped_rule,
                    "cap_bytes_per_sec": args.cap_bytes_per_sec,
                    "target_mbps": round(target_mbps, 3),
                    "within_plus_10pct": capped["mbps"] <= target_mbps * 1.10,
                }

            rendered = json.dumps(result, indent=2, sort_keys=True)
            if args.json_out:
                args.json_out.write_text(rendered + "\n", encoding="utf-8")
            print(rendered)
    finally:
        terminate_all(processes)

    return 0


if __name__ == "__main__":
    sys.exit(main())
