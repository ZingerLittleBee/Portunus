# TLS ClientHello fixtures (009-tls-sni-routing)

Real packet captures used by `crates/portunus-client/src/forwarder/sni/client_hello.rs`
unit tests. Phase 1 (T006) created the directory; T020..T024 in Phase 2
populated the actual `.bin` files.

## Layout

| File | TLS version | Bytes | Source | Notes |
|---|---|---|---|---|
| `client_hello_tls10.bin` | 1.0 | 122 | `openssl s_client -tls1` | T020 |
| `client_hello_tls11.bin` | 1.1 | 125 | `openssl s_client -tls1_1` | T021 |
| `client_hello_tls12.bin` | 1.2 | 209 | `openssl s_client -tls1_2` | T022 |
| `client_hello_tls13.bin` | 1.3 | 1469 | `openssl s_client -tls1_3` | T023; PQ-hybrid `X25519MLKEM768` keyshare included by default |
| `client_hello_fragmented.bin` | 1.3 | 1474 | hand-split from `client_hello_tls13.bin` | T024; see split offset below |

`servername` was set to `example.com` in every capture so the fixtures
exercise the SNI extension path.

## Capture procedure (reproducible)

These fixtures were captured with a one-shot `nc` listener — simpler
than running `openssl s_server` and avoids the certificate dance. The
listener records every byte the client sends; since the server never
responds, the file ends right after the ClientHello record.

```bash
# In a single shell, for each version. Use SECLEVEL=0 for TLS 1.0/1.1
# (OpenSSL 3.x disables them at the default security level):
PORT=19999
nc -l "$PORT" > out.bin &
NC=$!
sleep 0.3
echo "" | openssl s_client -connect "127.0.0.1:$PORT" \
    -tls1_2 \
    -servername example.com \
    -cipher 'DEFAULT@SECLEVEL=0' \
    -quiet 2>/dev/null &
sleep 0.6
kill "$NC" 2>/dev/null
# Verify: should start with `16 03 01` and the record-length bytes
# should match the file size minus 5.
od -An -tx1 -N5 out.bin
```

Captured against `OpenSSL 3.6.2 7 Apr 2026` on macOS Darwin 25.4.0
(Apple Silicon).

## Tradeoffs vs. `tcpdump` capture

The original procedure (preserved below for reference) used `tcpdump`
+ Wireshark/`tshark` to extract raw bytes. The `nc` shortcut above is
equivalent because the very first thing the TLS client writes on the
TCP connection IS the ClientHello — `nc` captures it byte-for-byte
with no headers to strip.

```bash
# Reference (tcpdump-based) procedure, preserved for completeness:
sudo tcpdump -i lo0 -w /tmp/tls12.pcap port 9999 &
openssl s_client -connect localhost:9999 -tls1_2 -servername example.com -quiet
sudo killall tcpdump
tshark -r /tmp/tls12.pcap -Y 'tls.handshake.type == 1' -T fields -e tcp.payload
```

## Fragmented fixture (T024)

`client_hello_fragmented.bin` is built by taking
`client_hello_tls13.bin` (a 1469-byte file containing one TLS record
of 1464 handshake-body bytes) and splitting that body at byte
**732** (the midpoint), wrapping each half in a fresh TLS record
header `16 03 01 <len_hi_lo>`. The result is two records back-to-back
totalling 1474 bytes. The parser's R-015 rule explicitly rejects
multi-record ClientHellos with `ParseError::Malformed`, so this
fixture is a **negative-test asset** — feeding it to `parse()` must
return `Err(Malformed)`, not `Truncated` or `Ok(_)`.

Reproduce the split:

```python
import struct
src = open("client_hello_tls13.bin", "rb").read()
record_len = struct.unpack(">H", src[3:5])[0]
body = src[5:5 + record_len]
half = len(body) // 2
def rec(payload):
    return b"\x16\x03\x01" + struct.pack(">H", len(payload)) + payload
open("client_hello_fragmented.bin", "wb").write(rec(body[:half]) + rec(body[half:]))
```
