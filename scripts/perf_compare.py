#!/usr/bin/env python3
"""Compare Portunus forwarding throughput against direct and in-kernel paths.

This harness measures, on a single host with `iperf3` on loopback:

1. Direct `iperf3` throughput to the target (baseline).
2. Linux `iptables` `nat/OUTPUT REDIRECT` throughput (in-kernel baseline).
3. `portunus-standalone` TCP forwarding (TOML-driven data plane).
4. `portunus-client` TCP forwarding (full control plane: server + enrollment
   + bidi gRPC rule push).

Paths 3 and 4 share the `portunus-forwarder` data plane end-to-end, so this
harness is also how we demonstrate that the standalone and client builds carry
the same forwarding cost. An optional offered-load sweep (`--offered-mbps`)
and a bandwidth-cap convergence check (`--cap-bytes-per-sec`) are reused from
the v1.3.0 `perf_loopback.py` flow.

It uses only the Python standard library plus an installed `iperf3` (and
`iptables` for `--with-iptables`) so it copies cleanly to a bare VPS.

Updated for the v1.6.1 CLI (one-time enrollment URI flow; SQLite `state.db`;
`server.toml` optional with `operator_token` auto-bootstrap of `_superadmin`).
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
    standalone_listen: int
    client_listen: int
    iptables_listen: int


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_for_metrics(port: int, deadline_seconds: float = 15.0) -> None:
    deadline = time.time() + deadline_seconds
    url = f"http://127.0.0.1:{port}/metrics"
    last_error: Exception | None = None
    while time.time() < deadline:
        try:
            urllib.request.urlopen(url, timeout=0.25).read()
            return
        except Exception as exc:  # noqa: BLE001 - keep startup context.
            last_error = exc
            time.sleep(0.1)
    raise RuntimeError(f"portunus-server did not expose {url}: {last_error}")


def tail_text(path: pathlib.Path, max_chars: int = 4000) -> str:
    if not path.exists():
        return "<missing log>"
    text = path.read_text(encoding="utf-8", errors="replace")
    return text if len(text) <= max_chars else text[-max_chars:]


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
        iperf3, "-J", "-c", "127.0.0.1", "-p", str(port),
        "-t", str(seconds), "-O", str(omit_seconds),
    ]
    if bitrate_mbps is not None:
        cmd.extend(["-b", f"{bitrate_mbps}M"])
    data = json.loads(check_output(cmd, os.environ.copy()))
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
        server.wait(timeout=seconds + 8)
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
        "-p", "tcp", "-d", "127.0.0.1", "--dport", str(listen_port),
        "-j", "REDIRECT", "--to-ports", str(target_port),
    ]


def add_iptables_redirect(iptables_bin: str, listen_port: int, target_port: int) -> list[str]:
    cmd_prefix = iptables_cmd(iptables_bin)
    rule = iptables_redirect_rule(listen_port, target_port)
    subprocess.check_call([*cmd_prefix, "-t", "nat", "-A", "OUTPUT", *rule])
    return [*cmd_prefix, "-t", "nat", "-D", "OUTPUT", *rule]


def terminate_all(processes: list[subprocess.Popen[Any]]) -> None:
    for proc in reversed(processes):
        if proc.poll() is not None:
            continue
        proc.terminate()
        try:
            proc.wait(timeout=4)
        except subprocess.TimeoutExpired:
            proc.kill()


def build_release(bins: list[pathlib.Path], packages: list[str]) -> None:
    if all(b.exists() for b in bins):
        return
    env = os.environ.copy()
    env.setdefault("PORTUNUS_SKIP_WEBUI", "1")
    cmd = ["cargo", "build", "--release"]
    for pkg in packages:
        cmd.extend(["-p", pkg])
    subprocess.check_call(cmd, cwd=ROOT, env=env)


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


# ─────────────────────────── standalone path ───────────────────────────


def write_standalone_config(path: pathlib.Path, listen_port: int, target_port: int) -> None:
    # Stats UDS disabled to avoid /run permission assumptions on a bare host.
    path.write_text(
        f"""[global]
label = "perf-bench"
log_level = "warn"
log_format = "compact"

[stats]
enabled = false

[[rule]]
name = "bench-tcp"
protocol = "tcp"
listen_port = {listen_port}
target = "127.0.0.1:{target_port}"
""",
        encoding="utf-8",
    )


def start_standalone(standalone_bin: pathlib.Path, config: pathlib.Path, log: pathlib.Path,
                     env: dict[str, str]) -> subprocess.Popen[Any]:
    return subprocess.Popen(
        [str(standalone_bin), "--config", str(config)],
        stdout=log.open("wb"),
        stderr=subprocess.STDOUT,
        env=env,
    )


# ───────────────────────────── client path ─────────────────────────────


def write_server_config(data_dir: pathlib.Path, ports: Ports, token: str) -> None:
    (data_dir / "server.toml").write_text(
        f"""control_listen = "127.0.0.1:{ports.control}"
operator_http_listen = "127.0.0.1:{ports.operator_http}"
metrics_listen = "127.0.0.1:{ports.metrics}"
operator_token = "{token}"
shutdown_drain_timeout_secs = 30
log_format = "compact"
range_rule_max_ports = 1024
""",
        encoding="utf-8",
    )


def create_enrollment_http(operator_port: int, token: str, name: str, address: str) -> str:
    # The offline `enroll-client` CLI refuses to open `state.db` while the
    # server holds the file lock (`store_in_use`), so create the enrollment
    # over the operator HTTP API instead. POST requires a same-origin
    # `Origin` header (CSRF) and a superadmin bearer token.
    origin = f"http://127.0.0.1:{operator_port}"
    payload = json.dumps({"name": name, "address": address}).encode("utf-8")
    req = urllib.request.Request(
        f"{origin}/v1/client-enrollments",
        data=payload,
        method="POST",
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
            "Origin": origin,
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            body = json.loads(resp.read())
    except urllib.error.HTTPError as exc:  # surface the server's error body
        detail = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"enrollment POST failed: {exc.code} {detail}") from exc
    return body["uri"]


# ───────────────────────────────── main ────────────────────────────────


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=int, default=10)
    parser.add_argument("--omit-seconds", type=int, default=2)
    parser.add_argument("--cap-bytes-per-sec", type=int, default=1_048_576,
                        help="Set 0 to skip the capped-rule convergence check.")
    parser.add_argument("--json-out", type=pathlib.Path)
    parser.add_argument("--iperf3", default=shutil.which("iperf3") or "iperf3")
    parser.add_argument("--offered-mbps",
                        help="Comma-separated TCP pacing rates, e.g. 100,500,1000.")
    parser.add_argument("--with-iptables", action="store_true",
                        help="On Linux, also benchmark nat/OUTPUT REDIRECT.")
    parser.add_argument("--iptables-bin", default=shutil.which("iptables") or "iptables")
    parser.add_argument("--skip-client", action="store_true",
                        help="Skip the full server+client control-plane path.")
    parser.add_argument("--skip-standalone", action="store_true",
                        help="Skip the portunus-standalone path.")
    parser.add_argument("--server-bin", type=pathlib.Path,
                        default=ROOT / "target/release/portunus-server")
    parser.add_argument("--client-bin", type=pathlib.Path,
                        default=ROOT / "target/release/portunus-client")
    parser.add_argument("--standalone-bin", type=pathlib.Path,
                        default=ROOT / "target/release/portunus-standalone")
    args = parser.parse_args()

    offered_mbps = parse_offered_mbps(args.offered_mbps)

    if not shutil.which(args.iperf3) and not pathlib.Path(args.iperf3).exists():
        raise SystemExit("iperf3 is required. Install it, or pass --iperf3 /path/to/iperf3.")
    if args.with_iptables:
        if platform.system() != "Linux":
            raise SystemExit("--with-iptables is only supported on Linux.")
        if not shutil.which(args.iptables_bin) and not pathlib.Path(args.iptables_bin).exists():
            raise SystemExit("iptables is required for --with-iptables.")

    packages: list[str] = []
    bins: list[pathlib.Path] = []
    if not args.skip_standalone:
        packages.append("portunus-standalone")
        bins.append(args.standalone_bin)
    if not args.skip_client:
        packages.extend(["portunus-server", "portunus-client"])
        bins.extend([args.server_bin, args.client_bin])
    if packages:
        build_release(bins, packages)

    ports = Ports(
        control=free_port(),
        operator_http=free_port(),
        metrics=free_port(),
        iperf_target=free_port(),
        standalone_listen=free_port(),
        client_listen=free_port(),
        iptables_listen=free_port(),
    )

    processes: list[subprocess.Popen[Any]] = []
    iptables_cleanup: list[str] | None = None
    result: dict[str, Any] = {}
    try:
        with tempfile.TemporaryDirectory(prefix="portunus-perf-") as tmp:
            tmpdir = pathlib.Path(tmp)
            env = os.environ.copy()
            env.setdefault("RUST_LOG", "warn")

            # 1. Direct baseline.
            direct = run_with_iperf_server(
                args.iperf3, ports.iperf_target, ports.iperf_target,
                args.seconds, args.omit_seconds,
            )

            result.update({
                "host": check_output_best_effort(["uname", "-a"], env),
                "rustc": check_output_best_effort(["rustc", "--version"], env),
                "iperf3": check_output_best_effort([args.iperf3, "--version"], env).splitlines()[0],
                "duration_seconds": args.seconds,
                "omit_seconds": args.omit_seconds,
                "direct": direct,
            })

            # 2. iptables REDIRECT baseline.
            if args.with_iptables:
                iptables_cleanup = add_iptables_redirect(
                    args.iptables_bin, ports.iptables_listen, ports.iperf_target,
                )
                iptables = run_with_iperf_server(
                    args.iperf3, ports.iperf_target, ports.iptables_listen,
                    args.seconds, args.omit_seconds,
                )
                result["iptables_redirect"] = iptables
                result["iptables_vs_direct_pct"] = pct(iptables["mbps"], direct["mbps"])

            # 3. portunus-standalone path.
            if not args.skip_standalone:
                cfg = tmpdir / "standalone.toml"
                write_standalone_config(cfg, ports.standalone_listen, ports.iperf_target)
                sa = start_standalone(
                    args.standalone_bin, cfg, tmpdir / "portunus-standalone.log", env,
                )
                processes.append(sa)
                time.sleep(1.5)
                if sa.poll() is not None:
                    raise RuntimeError(
                        "portunus-standalone exited early; log tail:\n"
                        + tail_text(tmpdir / "portunus-standalone.log")
                    )
                standalone = run_with_iperf_server(
                    args.iperf3, ports.iperf_target, ports.standalone_listen,
                    args.seconds, args.omit_seconds,
                )
                result["standalone_uncapped"] = standalone
                result["standalone_vs_direct_pct"] = pct(standalone["mbps"], direct["mbps"])
                if args.with_iptables:
                    result["standalone_vs_iptables_pct"] = pct(
                        standalone["mbps"], result["iptables_redirect"]["mbps"],
                    )

            # 4. portunus-client full control-plane path.
            if not args.skip_client:
                data_dir = tmpdir / "data"
                data_dir.mkdir()
                token = check_output([str(args.server_bin), "gen-token"], env).strip()
                write_server_config(data_dir, ports, token)
                env["PORTUNUS_OPERATOR_TOKEN"] = token

                server = subprocess.Popen(
                    [str(args.server_bin), "--data-dir", str(data_dir),
                     "--advertised-endpoint", f"127.0.0.1:{ports.control}", "serve"],
                    stdout=(tmpdir / "portunus-server.log").open("wb"),
                    stderr=subprocess.STDOUT, env=env,
                )
                processes.append(server)
                try:
                    wait_for_metrics(ports.metrics)
                except RuntimeError as exc:
                    if server.poll() is not None:
                        raise RuntimeError(
                            f"{exc}\nportunus-server exited {server.returncode}; log tail:\n"
                            + tail_text(tmpdir / "portunus-server.log")
                        ) from exc
                    raise

                # Client address is bare IP/hostname (no port) per the
                # operator API contract; it is metadata only for this bench.
                uri = create_enrollment_http(
                    ports.operator_http, token, "edge-01", "127.0.0.1",
                )
                bundle = tmpdir / "edge-01.bundle.json"
                check_output(
                    [str(args.client_bin), "enroll", uri, "--out", str(bundle)], env,
                )

                client = subprocess.Popen(
                    [str(args.client_bin), "--bundle", str(bundle),
                     "--stats-report-interval-secs", "1"],
                    stdout=(tmpdir / "portunus-client.log").open("wb"),
                    stderr=subprocess.STDOUT, env=env,
                )
                processes.append(client)
                time.sleep(2.0)

                client_rule = check_output(
                    [str(args.server_bin), "--data-dir", str(data_dir), "push-rule",
                     "edge-01", str(ports.client_listen), f"127.0.0.1:{ports.iperf_target}",
                     "--http-endpoint", f"127.0.0.1:{ports.operator_http}"],
                    env,
                ).strip()
                time.sleep(1.0)
                client_forward = run_with_iperf_server(
                    args.iperf3, ports.iperf_target, ports.client_listen,
                    args.seconds, args.omit_seconds,
                )
                result["client_uncapped"] = client_forward
                result["client_rule_id"] = client_rule
                result["client_vs_direct_pct"] = pct(client_forward["mbps"], direct["mbps"])
                if args.with_iptables:
                    result["client_vs_iptables_pct"] = pct(
                        client_forward["mbps"], result["iptables_redirect"]["mbps"],
                    )

            # standalone vs client head-to-head (shared portunus-forwarder).
            if not args.skip_standalone and not args.skip_client:
                result["standalone_vs_client_pct"] = pct(
                    result["standalone_uncapped"]["mbps"],
                    result["client_uncapped"]["mbps"],
                )

            # 5. Offered-load sweep across all available paths.
            if offered_mbps:
                matrix: list[dict[str, Any]] = []
                for offered in offered_mbps:
                    entry: dict[str, Any] = {"offered_mbps": offered}
                    entry["direct"] = run_with_iperf_server(
                        args.iperf3, ports.iperf_target, ports.iperf_target,
                        args.seconds, args.omit_seconds, offered,
                    )
                    if args.with_iptables:
                        entry["iptables_redirect"] = run_with_iperf_server(
                            args.iperf3, ports.iperf_target, ports.iptables_listen,
                            args.seconds, args.omit_seconds, offered,
                        )
                    if not args.skip_standalone:
                        entry["standalone"] = run_with_iperf_server(
                            args.iperf3, ports.iperf_target, ports.standalone_listen,
                            args.seconds, args.omit_seconds, offered,
                        )
                    if not args.skip_client:
                        entry["client"] = run_with_iperf_server(
                            args.iperf3, ports.iperf_target, ports.client_listen,
                            args.seconds, args.omit_seconds, offered,
                        )
                    matrix.append(entry)
                result["offered_mbps_matrix"] = matrix

            # 6. Bandwidth-cap convergence (standalone has no rule-push CLI, so
            #    this uses the client path when available).
            if args.cap_bytes_per_sec > 0 and not args.skip_client:
                capped_listen = free_port()
                capped_rule = check_output(
                    [str(args.server_bin), "--data-dir", str(data_dir), "push-rule",
                     "edge-01", str(capped_listen), "--target",
                     f"127.0.0.1:{ports.iperf_target}", "--bandwidth-in-bps",
                     str(args.cap_bytes_per_sec), "--http-endpoint",
                     f"127.0.0.1:{ports.operator_http}"],
                    env,
                ).strip()
                time.sleep(1.0)
                capped_seconds = max(args.seconds, 8)
                capped_omit = max(args.omit_seconds, 2)
                capped = run_with_iperf_server(
                    args.iperf3, ports.iperf_target, capped_listen,
                    capped_seconds, capped_omit,
                )
                target_mbps = args.cap_bytes_per_sec * 8 / 1_000_000
                result["client_capped"] = {
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
