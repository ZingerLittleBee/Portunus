# TLS ClientHello fixtures (009-tls-sni-routing)

Real packet captures used by `crates/forward-client/src/forwarder/sni/client_hello.rs`
unit tests. Phase 1 (T006) creates the directory; T020..T024 in Phase 2
populate the actual `.bin` files.

## Layout (filled by T020..T024)

| File | Source | Notes |
|---|---|---|
| `client_hello_tls10.bin` | `openssl s_client -tls1 -servername example.com` | T020 |
| `client_hello_tls11.bin` | `openssl s_client -tls1_1 …` | T021 |
| `client_hello_tls12.bin` | `openssl s_client -tls1_2 …` | T022 |
| `client_hello_tls13.bin` | `openssl s_client -tls1_3 …` | T023; PQ-hybrid `X25519MLKEM768` if available |
| `client_hello_fragmented.bin` | hand-spliced from one of the above | T024; document split offset below |

## Capture procedure (reproducible)

In one terminal, start an `openssl s_server`:

```bash
openssl s_server -accept 9999 -cert /tmp/test.crt -key /tmp/test.key -tls1_2
```

In another, run `s_client` and tcpdump the loopback:

```bash
sudo tcpdump -i lo0 -w /tmp/tls12.pcap port 9999 &
openssl s_client -connect localhost:9999 -tls1_2 -servername example.com -quiet
sudo killall tcpdump
```

Then extract the first ClientHello record (16 03 03 …) from the pcap with
your tool of choice (Wireshark "Export specified packets" → raw, or
`tshark -r tls12.pcap -Y 'tls.handshake.type == 1' -T fields -e tcp.payload`).

Save the raw bytes (without TCP / IP headers) into the corresponding
`.bin` file in this directory.

## Fragmented fixture (T024)

`client_hello_fragmented.bin` is built by taking `client_hello_tls13.bin`
and splitting it into two TLS records of equal-ish length, prepending a
fresh TLS record header to the second half. Document the byte offset
where the split happens here once T024 lands.
