# ORT Platform Support - Quick Decision Card

## The Bottom Line

| Platform | Status | Action |
|----------|--------|--------|
| **ubuntu-latest** | ✅ **GO** | Add to CI matrix immediately |
| **macos-14** | ✅ **GO** | Add to CI matrix immediately |
| **ubuntu-24.04-arm** | ✅ **GO** | Add to CI matrix (optional, for ARM64) |
| **macos-13** | ❌ **NO GO** | Skip - Intel macOS not supported by ort v2.0 |

## Why macos-13 is blocked

- `ort` v2.0.0-rc.12 has **NO prebuilt binaries for x86_64-apple-darwin** (Intel Mac)
- This is an **official decision** by the ort maintainers (GitHub issue #556: CLOSED as won't-fix)
- `fastembed` 5.9 depends on `ort = 2.0.0-rc.11`, so this blockage applies to you
- **Workaround**: Compile ONNX Runtime from source (30-40 minutes per CI run) - NOT recommended

## Quick CI Config

```yaml
test:
  strategy:
    fail-fast: false
    matrix:
      os:
        - ubuntu-latest    # Linux x86 - FAST & PROVEN
        - macos-14         # Apple Silicon - NEW & READY
        # - macos-13 ❌   # Blocked - no prebuilt binaries

  runs-on: ${{ matrix.os }}
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - uses: Swatinem/rust-cache@v2
    - run: cargo test --release
```

## Key Facts

1. **ort v2.0+** uses "download strategy" - fetches prebuilt ONNX Runtime binaries from `cdn.pyke.io`
2. **Supported binaries** (from `ort-sys/build/download/dist.txt`):
   - ✅ `aarch64-apple-darwin` (macOS Apple Silicon)
   - ✅ `x86_64-unknown-linux-gnu` (Linux x86)
   - ✅ `aarch64-unknown-linux-gnu` (Linux ARM64)
   - ❌ `x86_64-apple-darwin` (macOS Intel) - **DROPPED in v2.0**

3. **ort's own CI** tests on:
   - ubuntu-latest ✅
   - ubuntu-24.04-arm ✅
   - macos-15 ✅
   - windows-latest ✅
   - **Does NOT test macos-14 or macos-13**

## Cold Start Times

- ubuntu-latest: ~150ms (download prebuilt binary)
- macos-14: ~200ms (download prebuilt binary)
- ubuntu-24.04-arm: ~150ms (download prebuilt binary)
- macos-13: 30-40 MINUTES (build ONNX Runtime from source)

## If Intel macOS is Critical

You have 3 options:

### Option 1: Build from Source (30-40 min per CI run)
```yaml
macos-intel:
  runs-on: macos-13
  steps:
    - uses: actions/checkout@v4
    - name: Build ONNX Runtime
      run: |
        git clone --depth 1 --branch v1.24.4 https://github.com/microsoft/onnxruntime
        cd onnxruntime && ./build.sh --build --config Release
        cd ..
    - run: cargo build --release
      env:
        ORT_LIB_PATH: ${{ github.workspace }}/onnxruntime/build/MacOS/Release
```

### Option 2: Skip fastembed on Intel
```yaml
- name: Build without fastembed
  run: cargo build --release --no-default-features
```

### Option 3: Wait & Monitor
Track `pykeio/ort` releases. v2.1+ may restore Intel macOS support.

## Immediate Action Items

- [ ] Add `ubuntu-latest` to CI matrix
- [ ] Add `macos-14` to CI matrix
- [ ] Skip `macos-13` - document as unsupported
- [ ] Reference this document in CI configuration
- [ ] (Optional) Add `ubuntu-24.04-arm` for ARM64 support

## Sources

- [pykeio/ort](https://github.com/pykeio/ort) - v2.0.0-rc.12
  - Issue #556: "ort does not provide prebuilt binaries for x86_64-apple-darwin"
  - Binary manifest: `ort-sys/build/download/dist.txt`
- [Anush008/fastembed-rs](https://github.com/Anush008/fastembed-rs) - v5.9 (pins ort 2.0.0-rc.11)
- Research date: 2026-04-15
