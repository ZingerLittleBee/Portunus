# gRPC Cert / Domain Auto-Align Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans (inline). Steps use `- [ ]`.

**Goal:** When the server install gets `--domain` (or any explicit advertised endpoint), the server's self-signed gRPC cert SAN auto-covers that host and the value actually reaches the running server, so a single domain input "just works" end-to-end without changing the bundle-pin trust model.

**Architecture:** Server-side: thread an optional desired-SAN host through `tls.rs::load_or_generate`; a `.portunus-autocert` marker file distinguishes "ours, safe to regenerate" from "operator-supplied, hands-off" (no X.509 parsing — the marker records the baked extra host). `serve.rs` derives the host from `opts.advertised_endpoint` before cert load. Installer: precedence (explicit advertised > domain-derived), wizard reorder (domain first), and — necessary because `--advertised-endpoint`/`--operator-http-listen` have no env binding — switch the systemd drop-in to an `ExecStart=` override and append `--advertised-endpoint` to the docker compose `command:`.

**Tech Stack:** Rust (rcgen, tracing, std), Bash 4+, shellcheck, VPS smoke.

**Spec:** `docs/superpowers/specs/2026-05-18-grpc-domain-autoalign-design.md`

**Refinement vs spec (decided):** the spec mentioned parsing the leaf SAN (`leaf_san_covers`). To avoid adding an X.509-parsing dependency, the implementation instead records the baked extra-SAN host in the `.portunus-autocert` marker and compares against it. Same observable behavior: autogen certs self-heal on host change; operator-supplied certs (no marker) are never touched (a WARN is logged advising the operator to ensure their cert's SAN covers the host). The `advertised_host` helper mirrors the client's `extract_host` (`crates/portunus-client/src/control.rs:156`) **exactly** so the SAN host and the client's TLS verification host can never diverge; bare-IPv6-without-brackets is a known shared limitation, not in scope.

---

## File Structure

- `crates/portunus-server/src/tls.rs` — cert lifecycle. New: `desired_san` param on `load_or_generate`; `generate_self_signed(extra_host: Option<&str>)`; `push_host_san`; marker read/write; backup-on-regenerate; WARN logs. Owns all TLS-material logic.
- `crates/portunus-server/src/serve.rs` — `advertised_host()` helper; compute desired host from `opts.advertised_endpoint` and pass into `load_or_generate` (line 98).
- `scripts/install.sh` — `apply_advertised_default()` (precedence), wizard reorder (domain first), `render_dropin` → ExecStart override, compose `command:` advertised arg, `domain`-verb re-derive + re-issue note, `--effective-advertised` seam, one i18n key `adv_from_domain`.
- `scripts/install.test.sh` — network-free assertions.

---

### Task 1: tls.rs — SAN-aware generation + marker + signature change

**Files:**
- Modify: `crates/portunus-server/src/tls.rs` (`load_or_generate` 29-47, `generate_self_signed` 96-136, tests 185-237)

- [ ] **Step 1: Add the SAN helper + marker helpers (above `fn generate_self_signed`)**

Insert before line 96 (`fn generate_self_signed`):

```rust
fn marker_path(cert_path: &Path) -> PathBuf {
    cert_path.with_file_name(".portunus-autocert")
}

fn push_host_san(sans: &mut Vec<SanType>, host: &str) -> Result<(), PortunusError> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        sans.push(SanType::IpAddress(ip));
    } else {
        sans.push(SanType::DnsName(host.to_string().try_into().map_err(
            |e| PortunusError::Tls(format!("host '{host}' not valid for SAN: {e}")),
        )?));
    }
    Ok(())
}

fn backup_pair(cert_path: &Path, key_path: &Path) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for p in [cert_path, key_path] {
        let bak = std::path::PathBuf::from(format!("{}.portunus.{stamp}.bak", p.display()));
        let _ = std::fs::copy(p, &bak);
    }
}
```

- [ ] **Step 2: Change `generate_self_signed` to accept the extra host**

Replace the signature line 96 and the SAN block (lines 96-121) so it reads:

```rust
fn generate_self_signed(extra_host: Option<&str>) -> Result<ServerTlsMaterial, PortunusError> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    let hostname = hostname().unwrap_or_else(|| "portunus-server".to_string());
    let mut params = CertificateParams::default();
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = params.not_before + time::Duration::days(365 * 10);
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, hostname.clone());
    // SANs: webpki refuses certs with no SAN. Always cover the machine
    // hostname + loopback; additionally cover the advertised host so a
    // remote client verifying against it passes name validation.
    let mut sans = vec![
        SanType::DnsName(
            hostname
                .clone()
                .try_into()
                .map_err(|e| PortunusError::Tls(format!("hostname not valid for SAN: {e}")))?,
        ),
        SanType::DnsName("localhost".try_into().unwrap()),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
    ];
    if let Some(h) = extra_host {
        push_host_san(&mut sans, h)?;
    }
    params.subject_alt_names = sans;
```

(The remainder of `generate_self_signed` — key gen, `self_signed`, building `ServerTlsMaterial` — is unchanged from lines 122-136.)

- [ ] **Step 3: Rewrite `load_or_generate` with desired-SAN + marker logic**

Replace the whole `pub fn load_or_generate` body (lines 29-47) with:

```rust
    /// Load from disk, or generate fresh material if either file is
    /// missing. When `desired_san` is `Some(host)` and the on-disk pair
    /// is Portunus-autogenerated (a `.portunus-autocert` marker exists),
    /// regenerate if the marker's baked host differs from `host`.
    /// Operator-supplied pairs (no marker) are never modified.
    /// Refuses to start if `key_path` exists with perms broader than 0600.
    pub fn load_or_generate(
        cert_path: &Path,
        key_path: &Path,
        desired_san: Option<&str>,
    ) -> Result<Self, PortunusError> {
        let marker = marker_path(cert_path);
        if cert_path.exists() && key_path.exists() {
            enforce_key_perms(key_path)?;
            let cert_pem = std::fs::read_to_string(cert_path)?;
            let key_pem = std::fs::read_to_string(key_path)?;
            let der = leaf_der_from_pem(&cert_pem)?;
            let loaded = Self {
                leaf_fingerprint_hex: fingerprint::sha256_hex(&der),
                cert_pem,
                key_pem,
            };
            let Some(want) = desired_san else {
                return Ok(loaded);
            };
            if !marker.exists() {
                tracing::warn!(
                    event = "tls.operator_cert_in_use",
                    host = %want,
                    "operator-supplied {} present; ensure its SAN covers {want} \
                     (Portunus will not modify an operator cert)",
                    cert_path.display()
                );
                return Ok(loaded);
            }
            let baked = std::fs::read_to_string(&marker)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            if baked == want {
                return Ok(loaded);
            }
            backup_pair(cert_path, key_path);
            let material = generate_self_signed(Some(want))?;
            std::fs::write(cert_path, &material.cert_pem)?;
            write_secret(key_path, material.key_pem.as_bytes())?;
            std::fs::write(&marker, want)?;
            tracing::warn!(
                event = "tls.cert_regenerated",
                host = %want,
                old_fingerprint = %loaded.leaf_fingerprint_hex,
                new_fingerprint = %material.leaf_fingerprint_hex,
                "gRPC cert regenerated for new advertised host {want}; \
                 existing client bundles must be re-issued \
                 (portunus-server enroll-client …)"
            );
            Ok(material)
        } else {
            let material = generate_self_signed(desired_san)?;
            ensure_parent(cert_path)?;
            ensure_parent(key_path)?;
            std::fs::write(cert_path, &material.cert_pem)?;
            write_secret(key_path, material.key_pem.as_bytes())?;
            std::fs::write(&marker, desired_san.unwrap_or(""))?;
            Ok(material)
        }
    }
```

- [ ] **Step 4: Update the internal + test call sites to the new signatures**

Run: `grep -n "load_or_generate(&cert, &key)\|generate_self_signed()" crates/portunus-server/src/tls.rs`
Expected matches at ~191,196,208,220,235 (`load_or_generate`) — line 41's `generate_self_signed()` was already replaced in Step 3's `else` branch (now `generate_self_signed(desired_san)`).

For each `ServerTlsMaterial::load_or_generate(&cert, &key)` in the `#[cfg(test)] mod tests` block, add `, None`:
- line ~191: `let m1 = ServerTlsMaterial::load_or_generate(&cert, &key, None).unwrap();`
- line ~196: `let m2 = ServerTlsMaterial::load_or_generate(&cert, &key, None).unwrap();`
- line ~208: `ServerTlsMaterial::load_or_generate(&cert, &key, None).unwrap();`
- line ~220: `ServerTlsMaterial::load_or_generate(&cert, &key, None).unwrap();`
- line ~225: `let err = ServerTlsMaterial::load_or_generate(&cert, &key, None).unwrap_err();`
- line ~235: `let m = ServerTlsMaterial::load_or_generate(` … add `None` as the third arg before `.unwrap()` (multi-line call: the args are `&dir.path().join("…")` cert/key — append `,\n        None,`).

- [ ] **Step 5: Add new unit tests (end of `mod tests`, before its closing `}`)**

```rust
    #[test]
    fn generates_with_dns_extra_san() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");
        let m =
            ServerTlsMaterial::load_or_generate(&cert, &key, Some("portunus.example.com")).unwrap();
        assert!(m.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(dir.path().join(".portunus-autocert").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".portunus-autocert")).unwrap(),
            "portunus.example.com"
        );
    }

    #[test]
    fn autogen_regenerates_on_host_change() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");
        let a = ServerTlsMaterial::load_or_generate(&cert, &key, Some("a.example.com")).unwrap();
        let b = ServerTlsMaterial::load_or_generate(&cert, &key, Some("b.example.com")).unwrap();
        assert_ne!(a.leaf_fingerprint_hex, b.leaf_fingerprint_hex);
        let baks: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".portunus."))
            .collect();
        assert!(!baks.is_empty(), "expected a .bak backup pair");
    }

    #[test]
    fn autogen_idempotent_same_host() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");
        let a = ServerTlsMaterial::load_or_generate(&cert, &key, Some("x.example.com")).unwrap();
        let b = ServerTlsMaterial::load_or_generate(&cert, &key, Some("x.example.com")).unwrap();
        assert_eq!(a.leaf_fingerprint_hex, b.leaf_fingerprint_hex);
    }

    #[test]
    fn operator_cert_without_marker_never_regenerated() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");
        // Generate, then strip the marker to simulate an operator cert.
        let orig =
            ServerTlsMaterial::load_or_generate(&cert, &key, Some("old.example.com")).unwrap();
        std::fs::remove_file(dir.path().join(".portunus-autocert")).unwrap();
        let again =
            ServerTlsMaterial::load_or_generate(&cert, &key, Some("new.example.com")).unwrap();
        assert_eq!(orig.leaf_fingerprint_hex, again.leaf_fingerprint_hex);
    }

    #[test]
    fn ip_extra_san_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");
        let m = ServerTlsMaterial::load_or_generate(&cert, &key, Some("203.0.113.7")).unwrap();
        assert!(m.cert_pem.contains("BEGIN CERTIFICATE"));
    }
```

- [ ] **Step 6: Run the tls tests**

Run: `cargo test -p portunus-server --lib tls:: 2>&1 | tail -20`
Expected: all `tls::tests::*` pass (old + 5 new).

- [ ] **Step 7: Commit**

```bash
git add crates/portunus-server/src/tls.rs
git commit -m "feat(server): SAN-aware gRPC cert (marker-gated regen on advertised-host change)"
```

---

### Task 2: serve.rs — derive desired SAN host, thread into cert load

**Files:**
- Modify: `crates/portunus-server/src/serve.rs` (line 98 call; add helper near top of the module)

- [ ] **Step 1: Add the `advertised_host` helper**

Add (module-level, e.g. directly above `pub async fn run` or near the other free fns):

```rust
/// Host portion of an advertised `host:port` (or bare host). Mirrors the
/// client's `extract_host` exactly so the cert SAN host and the client's
/// TLS verification host can never diverge.
fn advertised_host(ep: &str) -> Option<String> {
    let ep = ep.trim();
    if ep.is_empty() {
        return None;
    }
    Some(
        ep.rsplit_once(':')
            .map_or_else(|| ep.to_string(), |(h, _)| h.to_string()),
    )
    .filter(|h| !h.is_empty())
}
```

- [ ] **Step 2: Pass the desired SAN into `load_or_generate`**

Replace line 98:

```rust
    let tls = ServerTlsMaterial::load_or_generate(&cfg.tls_cert_path, &cfg.tls_key_path)?;
```

with:

```rust
    let desired_san = opts.advertised_endpoint.as_deref().and_then(advertised_host);
    let tls = ServerTlsMaterial::load_or_generate(
        &cfg.tls_cert_path,
        &cfg.tls_key_path,
        desired_san.as_deref(),
    )?;
```

- [ ] **Step 3: Add a focused unit test for `advertised_host`**

Add to the existing `#[cfg(test)] mod tests` in `serve.rs` (find it with `grep -n "mod tests" crates/portunus-server/src/serve.rs`; if none exists, create `#[cfg(test)] mod tests { use super::*; … }` at end of file):

```rust
    #[test]
    fn advertised_host_extracts() {
        assert_eq!(advertised_host("d.example.com:7443").as_deref(), Some("d.example.com"));
        assert_eq!(advertised_host("203.0.113.7:7443").as_deref(), Some("203.0.113.7"));
        assert_eq!(advertised_host("bare.host").as_deref(), Some("bare.host"));
        assert_eq!(advertised_host(""), None);
        assert_eq!(advertised_host("   "), None);
    }
```

- [ ] **Step 4: Build + test**

Run: `cargo test -p portunus-server --lib 2>&1 | tail -15`
Expected: workspace server lib tests pass incl. `advertised_host_extracts`; no warnings (pedantic gate).

Run: `cargo clippy -p portunus-server --all-targets -- -D warnings 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/serve.rs
git commit -m "feat(server): derive gRPC cert SAN host from advertised endpoint"
```

---

### Task 3: installer — advertised precedence + `--effective-advertised` seam + i18n

**Files:**
- Modify: `scripts/install.sh`

- [ ] **Step 1: Add the i18n key (EN then ZH)**

In `MSG_EN`, after the line `  [https_public_note]=...` add:

```bash
  [adv_from_domain]="  advertised endpoint:  %s  (from domain)"
```

In `MSG_ZH`, after the corresponding `  [https_public_note]=...` add:

```bash
  [adv_from_domain]="  对外通告地址：%s（由域名推导）"
```

- [ ] **Step 2: Add the precedence helper**

Add near the other helpers (e.g. directly above `wizard_install`):

```bash
# Explicit --advertised-endpoint wins; otherwise a server --domain
# derives advertised = <domain>:7443. Idempotent; safe to call twice.
apply_advertised_default() {
  [ "$ROLE" = server ] || return 0
  [ -n "$DOMAIN" ] || return 0
  [ -z "$ADVERTISED" ] || return 0
  ADVERTISED="${DOMAIN}:7443"
  ADVERTISED_FROM_DOMAIN=yes
  return 0
}
```

Add `ADVERTISED_FROM_DOMAIN="no"` to the globals block (next to `DOMAIN=""`).

- [ ] **Step 3: Call it in `main()` and add the seam**

In `main()`, immediately after the existing line `[ -n "$DOMAIN" ] && [ "$ROLE" = client ] && die "--domain is server-only"` add:

```bash
  apply_advertised_default
  if [ "${PRINT_EFF:-no}" = yes ]; then printf '%s\n' "$ADVERTISED"; exit 0; fi
```

In `parse_args()` add a seam case after the `--effective-advertised`-adjacent seams (e.g. after `--render-dropin) … ;;`):

```bash
      --effective-advertised) PRINT_EFF=yes ;;
```

Add `PRINT_EFF="no"` near the other parse-time globals (next to `MENU_FORCE_STDIN="no"` or in the globals block).

- [ ] **Step 4: Show derivation in the summary**

In `print_install_summary`, find the server `advertised` lines:

```bash
    if [ -n "$ADVERTISED" ]; then
      t sum_advertised "$ADVERTISED $([ -n "$adv_prov" ] && t "$adv_prov")"; echo
    else
      t sum_advertised "$(t prov_loopback)"; echo
    fi
```

Replace with:

```bash
    if [ "$ADVERTISED_FROM_DOMAIN" = yes ]; then
      t adv_from_domain "$ADVERTISED"; echo
    elif [ -n "$ADVERTISED" ]; then
      t sum_advertised "$ADVERTISED $([ -n "$adv_prov" ] && t "$adv_prov")"; echo
    else
      t sum_advertised "$(t prov_loopback)"; echo
    fi
```

- [ ] **Step 5: shellcheck + i18n parity + commit**

Run: `shellcheck -s bash -S warning scripts/install.sh && diff <(bash scripts/install.sh --print-i18n-keys en|sort) <(bash scripts/install.sh --print-i18n-keys zh|sort) && echo OK`
Expected: `OK`.

```bash
git add scripts/install.sh
git commit -m "feat(install): advertised precedence (explicit > domain-derived) + seam + i18n"
```

---

### Task 4: installer — wizard reorder (domain first, skip advertised when set)

**Files:**
- Modify: `scripts/install.sh` (`wizard_install` server branch)

- [ ] **Step 1: Reorder the server prompts**

In `wizard_install`, the server branch currently asks the advertised loop, then the domain loop. Replace the whole server `if [ "$ROLE" = server ]; then … fi` body (the `detect_public_ip`/`adv_prov`/advertised `while` loop and the domain `while` loop) with:

```bash
  if [ "$ROLE" = server ]; then
    while :; do
      DOMAIN="$(ask ask_domain)"
      [ -z "$DOMAIN" ] && break
      valid_fqdn "$DOMAIN" && break
      t bad_domain "$DOMAIN"; echo
    done
    if [ -n "$DOMAIN" ]; then
      ADVERTISED="${DOMAIN}:7443"; ADVERTISED_FROM_DOMAIN=yes
    else
      detect_public_ip
      adv_prov="$DETECTED_PROV"
      while :; do
        a="$(ask ask_advertised_pub "${DETECTED_IP}:7443")"
        if [ -z "$a" ]; then ADVERTISED="${DETECTED_IP}:7443"; break; fi
        if [ "$a" = "-" ]; then ADVERTISED=""; adv_prov="prov_loopback"; break; fi
        if valid_host_port "$a"; then ADVERTISED="$a"; adv_prov="prov_user"; break; fi
        t bad_endpoint "$a"; echo
      done
    fi
  fi
```

- [ ] **Step 2: shellcheck + wizard seam smoke**

Run: `shellcheck -s bash -S warning scripts/install.sh`
Expected: clean.

Run (domain entered ⇒ advertised auto, no advertised prompt):
```bash
printf '1\n2\nserverbee-test.900040.xyz\n\n0\n' | PORTUNUS_SKIP_IP_PROBE=1 bash scripts/install.sh --menu-stdin --lang en 2>&1 | grep -i "advertised endpoint"
```
Expected: a summary line showing `serverbee-test.900040.xyz:7443  (from domain)`; NO "Public advertised endpoint" prompt appeared.

Run (blank domain ⇒ advertised prompt asked):
```bash
printf '1\n2\n\n\n0\n' | PORTUNUS_SKIP_IP_PROBE=1 bash scripts/install.sh --menu-stdin --lang en 2>&1 | grep -i "advertised"
```
Expected: the `Public advertised endpoint [...]` prompt IS present.

- [ ] **Step 3: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(install): wizard asks domain first; derives advertised, skips the prompt"
```

---

### Task 5: installer — ExecStart-override drop-in + compose advertised arg

**Files:**
- Modify: `scripts/install.sh` (`render_dropin` 405-410; `write_compose_file` compose `command:`)

- [ ] **Step 1: Replace `render_dropin` with an ExecStart override**

Replace the whole `render_dropin()` function (lines 405-410) with:

```bash
render_dropin() {
  local dd="${DATA_DIR:-/var/lib/portunus}" args
  args="--data-dir ${dd} serve"
  [ -n "$OP_HTTP_LISTEN" ] && args="${args} --operator-http-listen ${OP_HTTP_LISTEN}"
  [ -n "$ADVERTISED" ]     && args="${args} --advertised-endpoint ${ADVERTISED}"
  printf '[Service]\nExecStart=\nExecStart=/usr/local/bin/portunus-server %s\n' "$args"
  return 0
}
```

(`ExecStart=` cleared then re-set is the standard systemd drop-in override; the inert `Environment=PORTUNUS_*` lines are removed — the server does not read them.)

- [ ] **Step 2: Append `--advertised-endpoint` to the docker compose command**

In `write_compose_file`, locate the `command:` line:

```bash
    command: ["--data-dir", "/var/lib/portunus", "serve", "--operator-http-listen", "0.0.0.0:${port}"]
```

Change the function to build the command array with an optional advertised element. Replace that single `command:` line by first computing, just before the `cat > "$f" <<YAML` line, an `advcmd` fragment:

```bash
  local advcmd=""
  [ -n "$ADVERTISED" ] && advcmd=", \"--advertised-endpoint\", \"${ADVERTISED}\""
```

and changing the heredoc `command:` line to:

```bash
    command: ["--data-dir", "/var/lib/portunus", "serve", "--operator-http-listen", "0.0.0.0:${port}"${advcmd}]
```

- [ ] **Step 3: shellcheck + render-dropin seam check**

Run: `shellcheck -s bash -S warning scripts/install.sh`
Expected: clean.

Run:
```bash
ADVTEST() { ADVERTISED=d.example.com:7443 DATA_DIR=/var/lib/portunus OP_HTTP_LISTEN=127.0.0.1:7080 bash scripts/install.sh --render-dropin; }
ADVTEST
```
Expected output exactly:
```
[Service]
ExecStart=
ExecStart=/usr/local/bin/portunus-server --data-dir /var/lib/portunus serve --operator-http-listen 127.0.0.1:7080 --advertised-endpoint d.example.com:7443
```
(No `Environment=PORTUNUS_ADVERTISED_ENDPOINT=` anywhere.)

- [ ] **Step 4: Commit**

```bash
git add scripts/install.sh
git commit -m "fix(install): pass advertised/op-http as real CLI args (systemd ExecStart override + compose command)"
```

---

### Task 6: installer — `domain` verb re-derive + re-issue note

**Files:**
- Modify: `scripts/install.sh` (`lifecycle_domain`)

- [ ] **Step 1: Re-derive advertised + print the re-issue note in `lifecycle_domain`**

In `lifecycle_domain`, after `setup_caddy_domain` and before/around the existing `meta_write`, ensure the advertised endpoint is re-derived and the operator is told to re-issue bundles. Locate the `setup_caddy_domain` call inside `lifecycle_domain` and immediately after it add:

```bash
  apply_advertised_default
  if [ "$DRY_RUN" != yes ] && [ -n "$ADVERTISED" ]; then
    if [ "$DEPLOY" = docker ]; then
      COMPOSE_DIR="$(dirname "$mf")"; write_compose_file "$COMPOSE_DIR"; write_compose_env "$COMPOSE_DIR"
      ( cd "$COMPOSE_DIR" && $(compose_cmd) up -d ) || true
    else
      write_server_dropin
      command -v systemctl >/dev/null 2>&1 && sudo systemctl restart "portunus-$ROLE" 2>/dev/null || true
    fi
    echo "→ advertised endpoint set to ${ADVERTISED}; the server will re-align its gRPC cert SAN on restart."
    echo "→ existing client bundles must be re-issued: portunus-server enroll-client <name>"
  fi
```

(`mf`, `ROLE`, `DEPLOY` are already locals in `lifecycle_domain`; `DOMAIN` is set by the verb so `apply_advertised_default` fills `ADVERTISED` unless the operator passed an explicit one.)

- [ ] **Step 2: shellcheck + dry-run check**

Run: `shellcheck -s bash -S warning scripts/install.sh`
Expected: clean.

Run: `bash scripts/install.sh domain serverbee-test.900040.xyz --skip-dns-check --dry-run; echo "exit=$?"`
Expected: `exit=0`, no writes (dry-run short-circuits in `main`).

- [ ] **Step 3: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(install): domain verb re-derives advertised, rewrites unit/compose, prints re-issue note"
```

---

### Task 7: network-free assertions

**Files:**
- Modify: `scripts/install.test.sh` (before the shellcheck block); update any prior `--render-dropin` expectation

- [ ] **Step 1: Update/replace the old render-dropin assertion**

Run: `grep -n "render-dropin\|Environment=PORTUNUS\|ExecStart" scripts/install.test.sh`
For any existing assertion expecting `Environment=PORTUNUS_*` from `--render-dropin`, replace it with the new expectation:

```bash
# --- render-dropin is now an ExecStart override ---
rd="$(ADVERTISED=d.example.com:7443 DATA_DIR=/var/lib/portunus OP_HTTP_LISTEN=127.0.0.1:7080 bash "$script" --render-dropin)"
printf '%s\n' "$rd" | grep -qx 'ExecStart=' || fail "missing ExecStart= clear line"
printf '%s\n' "$rd" | grep -q 'ExecStart=/usr/local/bin/portunus-server --data-dir /var/lib/portunus serve --operator-http-listen 127.0.0.1:7080 --advertised-endpoint d.example.com:7443' || fail "missing ExecStart override"
printf '%s\n' "$rd" | grep -q 'Environment=PORTUNUS_ADVERTISED_ENDPOINT=' && fail "inert Environment line still emitted" || true
```

- [ ] **Step 2: Add precedence + seam + compose assertions**

Add before the shellcheck block:

```bash
# --- advertised precedence: domain derives, explicit wins ---
ea1="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --domain d.example.com --effective-advertised 2>/dev/null)"
[ "$ea1" = "d.example.com:7443" ] || fail "domain should derive advertised d.example.com:7443 (got '$ea1')"
ea2="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --domain d.example.com --advertised-endpoint x.host:7443 --effective-advertised 2>/dev/null)"
[ "$ea2" = "x.host:7443" ] || fail "explicit advertised must win (got '$ea2')"
ea3="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --effective-advertised 2>/dev/null)"
[ -z "$ea3" ] || fail "no domain/explicit ⇒ empty effective advertised (got '$ea3')"

# --- dry-run plan shows the derived advertised ---
dp="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --domain d.example.com --skip-dns-check --dry-run 2>&1)"
printf '%s\n' "$dp" | grep -q 'advertised:[[:space:]]*d.example.com:7443' || fail "dry-run plan missing derived advertised"

# --- docker compose command carries --advertised-endpoint ---
cdir="$(mktemp -d)"
( cd "$cdir" && ADVERTISED=d.example.com:7443 ROLE=server DEPLOY=docker bash -c '
  set -e; source "'"$script"'" --source-only 2>/dev/null || true' ) 2>/dev/null || true
# Direct functional check via the seam-free path: render compose by install dry-run is not available,
# so assert the template logic by sourcing is avoided; instead assert via a real (offline) write:
rm -rf "$cdir"; cdir="$(mktemp -d)"
PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --deploy docker --compose-dir "$cdir" --domain d.example.com --skip-dns-check --dry-run >/dev/null 2>&1 || true
# dry-run writes nothing; compose-content correctness is covered by the VPS smoke (Task 8 step 8).
rm -rf "$cdir"
```

(Note: the compose `command:` content is verified for real in the VPS smoke step 8; network-free we only assert the dry-run path stays write-free, consistent with the existing harness philosophy.)

- [ ] **Step 3: Run full harness + Rust tests + clippy**

Run:
```bash
bash scripts/install.test.sh 2>&1 | tail -1 \
 && cargo test -p portunus-server --lib 2>&1 | tail -1 \
 && cargo clippy -p portunus-server --all-targets -- -D warnings 2>&1 | tail -1 \
 && echo ALLGREEN
```
Expected: `PASS` … test result `ok` … clippy clean … `ALLGREEN`.

- [ ] **Step 4: Commit**

```bash
git add scripts/install.test.sh
git commit -m "test: advertised precedence/seam + ExecStart-override network-free assertions"
```

---

### Task 8: VPS smoke — all 10 design points

**Files:** none (remote). Helpers `/tmp/vps.sh`, `/tmp/vpstty.sh`. Domain `serverbee-test.900040.xyz` → `207.241.173.217`.

Build the release server binary is NOT required (VPS pulls released v1.4.2 image/binary). The cert/SAN change is in the server binary — **the VPS must run a build that includes Task 1/2**. Since releases lag, build the linux server binary locally and stage it, OR run the smoke against a locally-built binary uploaded to the VPS.

- [ ] **Step 1: Build the server binary for linux and upload**

```bash
cargo build -p portunus-server --release --target x86_64-unknown-linux-gnu 2>&1 | tail -3 || \
  cargo build -p portunus-server --release 2>&1 | tail -3   # fallback if cross target absent
```
Upload the resulting `portunus-server` and `scripts/install.sh`/`scripts/install.test.sh` to `/root/smoke/` on the VPS. If a linux cross-build is unavailable on this host, instead `cargo build` inside an ephemeral VPS dir (`/root/smoke/build`) by uploading the repo crates — choose whichever is feasible; record which path was used.

- [ ] **Step 2** Network-free harness on the VPS ⇒ `PASS`.

- [ ] **Step 3 (design pt 2)** Binary install `--domain serverbee-test.900040.xyz` (no explicit advertised). Verify: the systemd drop-in is the `ExecStart=` override containing `--advertised-endpoint serverbee-test.900040.xyz:7443`; start the service; `openssl x509 -in /var/lib/portunus/server.crt -noout -text | grep -A1 'Subject Alternative Name'` contains `DNS:serverbee-test.900040.xyz`; `/var/lib/portunus/.portunus-autocert` == the domain; Web UI `https://serverbee-test.900040.xyz/` still 200 (Caddy unaffected).

- [ ] **Step 4 (design pt 3 — the crux)** Real client enrollment over the domain:
  `portunus-server --data-dir /var/lib/portunus enroll-client edge01 --address serverbee-test.900040.xyz` (or the operator-API path) → obtain bundle → run the uploaded `portunus-client` with it → confirm `control.connected` in client logs (TLS verified against the domain SAN via the bundle pin). Push a TCP rule and pass bytes through to prove the data plane works end-to-end.

- [ ] **Step 5 (design pt 4 — precedence)** Re-install with `--domain serverbee-test.900040.xyz --advertised-endpoint 207.241.173.217:7443`. Verify cert SAN now contains `IP Address:207.241.173.217`, drop-in advertises the IP, marker == `207.241.173.217`; Web UI still uses the domain via Caddy.

- [ ] **Step 6 (design pt 5 — idempotent)** `systemctl restart portunus-server`; capture fingerprint before/after (`openssl x509 -in … -fingerprint -sha256 -noout`): unchanged; no new `*.portunus.*.bak`.

- [ ] **Step 7 (design pt 6 — re-point)** `install.sh domain <alt fqdn that also A-records to the VPS>` OR re-run with the original domain after the IP step: expect `tls.cert_regenerated` WARN in `journalctl -u portunus-server`, a `.bak` pair written, marker updated, installer printed the re-issue note. Old bundle now fails to connect; a freshly issued bundle connects.

- [ ] **Step 8 (design pt 7 — operator cert)** Stop server; replace `/var/lib/portunus/server.crt`+`.key` with a hand-rolled self-signed (any SAN, e.g. CN=foo) and `rm /var/lib/portunus/.portunus-autocert`; start server. Expect: starts OK, `journalctl` shows `tls.operator_cert_in_use` WARN, the cert is byte-unchanged (fingerprint stable), no `.bak` created.

- [ ] **Step 9 (design pt 8 — docker)** Docker install `--deploy docker --compose-dir /root/dk --domain serverbee-test.900040.xyz` using the locally-built image OR (if no custom image) document that docker uses the released image and therefore exercises only the compose-command wiring + Caddy, with the SAN behavior covered by the binary path. Verify the generated `compose.yml` `command:` contains `--advertised-endpoint serverbee-test.900040.xyz:7443`; container up; Web UI 200. If a custom server image with Task 1/2 is buildable, additionally repeat Step 4's client enrollment against the container.

- [ ] **Step 10 (design pt 9 — wizard)** PTY wizard: enter the domain at the first prompt → advertised prompt skipped, end state == Step 3 (verify cert SAN + drop-in). Second PTY run, blank domain → advertised prompt asked, no domain SAN, marker == "".

- [ ] **Step 11 (design pt 10 — regression)** Install with neither `--domain` nor `--advertised-endpoint`: drop-in `ExecStart` has no `--advertised-endpoint`; server logs advertised loopback; `server.crt` SAN has no extra host; marker == "". `--dry-run` performs zero writes. Then **full host cleanup** (stop+disable service, remove binaries/units/drop-ins/containers/volumes/`/var/lib/portunus`/`/etc/caddy` block/`apt-get remove -y caddy`/`/root/smoke`/`/root/dk`/`~/.config/portunus`); verify clean.

- [ ] **Step 12** Fix any smoke-found issues (re-run affected steps until green), then commit fixes:

```bash
git add -A && git commit -m "fix: gRPC domain auto-align — VPS smoke-driven fixes"
```

---

## Self-Review

**Spec coverage:**
- Core rule (SAN covers advertised host; IP vs DNS; regen autogen; warn operator) → Task 1 (`generate_self_signed`/`load_or_generate`/`push_host_san`/marker) + Task 8 steps 3,5,7,8.
- Autogen marker semantics → Task 1 Step 3 + tests Step 5; smoke steps 3,7,8.
- Re-pin/bundle-reissue surfacing → Task 1 WARN logs + Task 6 installer note; smoke step 7.
- Precedence (explicit > domain) → Task 3 `apply_advertised_default` + tests Task 7; smoke step 5.
- Threading to running server (ExecStart override / compose command) → Task 5; tests Task 7; smoke steps 3,9,11.
- Wizard reorder → Task 4; smoke step 10.
- `domain` verb re-derive + note → Task 6; smoke step 7.
- Error handling (FQDN reuse, key perms preserved, backups) → Task 1 (`backup_pair`, `write_secret`/`enforce_key_perms` preserved); design FQDN validation already in the Caddy `valid_fqdn`.
- Testing (Rust unit, network-free, VPS 10 pts) → Tasks 1/2/7/8.
- Regression byte-identical no-flags → Task 8 step 11.

**Placeholder scan:** none — every code step has complete code; the only deferred verification (docker compose content) is explicitly justified and covered by the VPS smoke, not left vague.

**Type/name consistency:** `load_or_generate(cert,key,desired_san: Option<&str>)` defined Task 1 Step 3, called Task 1 Step 4 (tests, `None`), Task 2 Step 2 (`desired_san.as_deref()`). `generate_self_signed(extra_host: Option<&str>)` defined/used Task 1. `advertised_host` defined Task 2 Step 1, used Step 2. Installer: `apply_advertised_default` / `ADVERTISED_FROM_DOMAIN` / `PRINT_EFF` defined Task 3, used Tasks 3/4/6. `render_dropin` ExecStart form Task 5, asserted Task 7. Marker filename `.portunus-autocert` consistent across Task 1 + smoke steps 3,8,10,11.
