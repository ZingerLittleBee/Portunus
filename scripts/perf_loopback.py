#!/usr/bin/env python3
"""Run a reproducible local Portunus performance smoke benchmark.

The harness starts a real `forward-server`, a real `forward-client`, and
`iperf3` on loopback. It measures:

1. Direct iperf3 throughput to the target.
2. Throughput through a pushed Portunus TCP rule.
3. Optional ingress bandwidth-cap convergence through a second pushed rule.
4. Optional Linux iptables REDIRECT throughput for an in-kernel baseline.

It intentionally uses only the Python standard library plus an installed
`iperf3` binary so operators can copy the workflow to bare VPS hosts.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import platform
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
    iptables_listen: int


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


def tail_text(path: pathlib.Path, max_chars: int = 4000) -> str:
    if not path.exists():
        return "<missing log>"
    text = path.read_text(encoding="utf-8", errors="replace")
    if len(text) <= max_chars:
        return text
    return text[-max_chars:]


def check_output(cmd: list[str], env: dict[str, str]) -> str:
    return subprocess.check_output(cmd, env=env, stderr=subprocess.STDOUT, text=True)


def check_output_best_effort(cmd: list[str], env: dict[str, str]) -> str:
    try:
        return check_output(cmd, env).strip()
    except (OSError, subprocess.CalledProcessError) as exc:
        return f"unavailable ({exc})"


def run_iperf3(
    iperf3: str,
    port: int,
    seconds: int,
    omit_seconds: int,
    bitrate_mbps: int | None = None,
) -> dict[str, Any]:
    cmd = [
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
    ]
    if bitrate_mbps is not None:
        cmd.extend(["-b", f"{bitrate_mbps}M"])
    output = check_output(
        cmd,
        os.environ.copy(),
    )
    data = json.loads(output)
    received = data["end"]["sum_received"]["bits_per_second"]
    sent = data["end"]["sum_sent"]
    return {
        "mbps": round(received / 1_000_000, 3),
        "retransmits": sent.get("retransmits"),
    }


def pct(numerator_mbps: float, denominator_mbps: float) -> float | None:
    if denominator_mbps <= 0.0:
        return None
    return round(numerator_mbps / denominator_mbps * 100.0, 2)


def start_iperf3_server(iperf3: str, port: int) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [iperf3, "-s", "-p", str(port), "-1"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.STDOUT,
    )


def run_with_iperf_server(
    iperf3: str,
    target_port: int,
    client_port: int,
    seconds: int,
    omit_seconds: int,
    bitrate_mbps: int | None = None,
) -> dict[str, Any]:
    server = start_iperf3_server(iperf3, target_port)
    try:
        time.sleep(0.3)
        result = run_iperf3(iperf3, client_port, seconds, omit_seconds, bitrate_mbps)
        server.wait(timeout=seconds + 5)
        return result
    finally:
        if server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=3)
            except subprocess.TimeoutExpired:
                server.kill()


def iptables_cmd(iptables_bin: str) -> list[str]:
    if os.geteuid() == 0:
        return [iptables_bin]
    sudo = shutil.which("sudo")
    if sudo is None:
        raise RuntimeError("iptables baseline requires root or sudo")
    return [sudo, "-n", iptables_bin]


def iptables_redirect_rule(listen_port: int, target_port: int) -> list[str]:
    # Local loopback clients traverse nat/OUTPUT, not PREROUTING.
    return [
        "-p",
        "tcp",
        "-d",
        "127.0.0.1",
        "--dport",
        str(listen_port),
        "-j",
        "REDIRECT",
        "--to-ports",
        str(target_port),
    ]


def add_iptables_redirect(iptables_bin: str, listen_port: int, target_port: int) -> list[str]:
    cmd_prefix = iptables_cmd(iptables_bin)
    rule = iptables_redirect_rule(listen_port, target_port)
    table_and_chain = ["-t", "nat", "OUTPUT"]
    subprocess.check_call([*cmd_prefix, "-t", "nat", "-A", "OUTPUT", *rule])
    return [*cmd_prefix, *table_and_chain[:2], "-D", table_and_chain[2], *rule]


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


def parse_offered_mbps(raw: str | None) -> list[int]:
    if raw is None or raw.strip() == "":
        return []
    out: list[int] = []
    for part in raw.split(","):
        value = int(part.strip())
        if value <= 0:
            raise ValueError("--offered-mbps values must be positive")
        out.append(value)
    return out


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
    parser.add_argument(
        "--offered-mbps",
        help=(
            "Comma-separated TCP application pacing rates to compare across "
            "direct, Portunus, and iptables paths, e.g. 100,500,1000."
        ),
    )
    parser.add_argument(
        "--with-iptables",
        action="store_true",
        help="On Linux, also benchmark nat/OUTPUT REDIRECT as an in-kernel baseline.",
    )
    parser.add_argument("--iptables-bin", default=shutil.which("iptables") or "iptables")
    parser.add_argument(
        "--server-bin",
        type=pathlib.Path,
        default=ROOT / "target/release/forward-server",
    )
    parser.add_argument(
        "--client-bin",
        type=pathlib.Path,
        default=ROOT / "target/release/forward-client",
    )
    args = parser.parse_args()
    offered_mbps = parse_offered_mbps(args.offered_mbps)

    if not shutil.which(args.iperf3) and not pathlib.Path(args.iperf3).exists():
        raise SystemExit("iperf3 is required. Install it, or pass --iperf3 /path/to/iperf3.")
    if args.with_iptables:
        if platform.system() != "Linux":
            raise SystemExit("--with-iptables is only supported on Linux.")
        if not shutil.which(args.iptables_bin) and not pathlib.Path(args.iptables_bin).exists():
            raise SystemExit(
                "iptables is required for --with-iptables. "
                "Install it, or pass --iptables-bin /path/to/iptables."
            )
    if offered_mbps and not args.with_iptables:
        raise SystemExit("--offered-mbps requires --with-iptables.")

    build_release(args.server_bin, args.client_bin)

    ports = Ports(
        control=free_port(),
        operator_http=free_port(),
        metrics=free_port(),
        iperf_target=free_port(),
        uncapped_listen=free_port(),
        capped_listen=free_port(),
        iptables_listen=free_port(),
    )

    processes: list[subprocess.Popen[Any]] = []
    iptables_cleanup: list[str] | None = None
    try:
        with tempfile.TemporaryDirectory(prefix="portunus-perf-") as tmp:
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
                stdout=(tmpdir / "forward-server.log").open("wb"),
                stderr=subprocess.STDOUT,
                env=env,
            )
            processes.append(server)
            try:
                wait_for_metrics(ports.metrics)
            except RuntimeError as exc:
                if server.poll() is not None:
                    log_tail = tail_text(tmpdir / "forward-server.log")
                    raise RuntimeError(
                        f"{exc}\nforward-server exited with {server.returncode}; log tail:\n{log_tail}"
                    ) from exc
                raise

            client = subprocess.Popen(
                [
                    str(args.client_bin),
                    "--bundle",
                    str(bundle),
                    "--stats-report-interval-secs",
                    "1",
                ],
                stdout=(tmpdir / "forward-client.log").open("wb"),
                stderr=subprocess.STDOUT,
                env=env,
            )
            processes.append(client)
            time.sleep(1.5)

            direct = run_with_iperf_server(
                args.iperf3,
                ports.iperf_target,
                ports.iperf_target,
                args.seconds,
                args.omit_seconds,
            )

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
            uncapped = run_with_iperf_server(
                args.iperf3,
                ports.iperf_target,
                ports.uncapped_listen,
                args.seconds,
                args.omit_seconds,
            )

            result: dict[str, Any] = {
                "host": check_output(["uname", "-a"], env).strip(),
                "rustc": check_output_best_effort(["rustc", "--version"], env),
                "iperf3": check_output_best_effort([args.iperf3, "--version"], env).splitlines()[0],
                "duration_seconds": args.seconds,
                "omit_seconds": args.omit_seconds,
                "direct": direct,
                "portunus_uncapped": uncapped,
                "portunus_vs_direct_pct": pct(uncapped["mbps"], direct["mbps"]),
                "uncapped_rule_id": uncapped_rule,
            }

            if args.with_iptables:
                iptables_cleanup = add_iptables_redirect(
                    args.iptables_bin,
                    ports.iptables_listen,
                    ports.iperf_target,
                )
                iptables = run_with_iperf_server(
                    args.iperf3,
                    ports.iperf_target,
                    ports.iptables_listen,
                    args.seconds,
                    args.omit_seconds,
                )
                result["iptables_redirect"] = iptables
                result["iptables_vs_direct_pct"] = pct(iptables["mbps"], direct["mbps"])
                result["portunus_vs_iptables_pct"] = pct(uncapped["mbps"], iptables["mbps"])

            if offered_mbps:
                matrix: list[dict[str, Any]] = []
                for offered in offered_mbps:
                    direct_limited = run_with_iperf_server(
                        args.iperf3,
                        ports.iperf_target,
                        ports.iperf_target,
                        args.seconds,
                        args.omit_seconds,
                        offered,
                    )
                    forward_limited = run_with_iperf_server(
                        args.iperf3,
                        ports.iperf_target,
                        ports.uncapped_listen,
                        args.seconds,
                        args.omit_seconds,
                        offered,
                    )
                    iptables_limited = run_with_iperf_server(
                        args.iperf3,
                        ports.iperf_target,
                        ports.iptables_listen,
                        args.seconds,
                        args.omit_seconds,
                        offered,
                    )
                    matrix.append(
                        {
                            "offered_mbps": offered,
                            "direct": direct_limited,
                            "portunus_uncapped": forward_limited,
                            "iptables_redirect": iptables_limited,
                            "portunus_vs_direct_pct": pct(
                                forward_limited["mbps"],
                                direct_limited["mbps"],
                            ),
                            "portunus_vs_iptables_pct": pct(
                                forward_limited["mbps"],
                                iptables_limited["mbps"],
                            ),
                            "iptables_vs_direct_pct": pct(
                                iptables_limited["mbps"],
                                direct_limited["mbps"],
                            ),
                        }
                    )
                result["offered_mbps_matrix"] = matrix

            if args.cap_bytes_per_sec > 0:
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
                capped_seconds = max(args.seconds, 8)
                capped_omit = max(args.omit_seconds, 2)
                capped = run_with_iperf_server(
                    args.iperf3,
                    ports.iperf_target,
                    ports.capped_listen,
                    capped_seconds,
                    capped_omit,
                )
                target_mbps = args.cap_bytes_per_sec * 8 / 1_000_000
                result["portunus_capped"] = {
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
        if iptables_cleanup is not None:
            subprocess.run(iptables_cleanup, check=False)
        terminate_all(processes)

    return 0


if __name__ == "__main__":
    sys.exit(main())
