# Matter over Thread Setup & Troubleshooting Guide

This document preserves the exact setup steps, toolchain requirements, and code modifications required to compile and commission the `matter_thread_light` example on the **Adafruit Feather ESP32-C6** using Rust and OpenThread.

---

## 1. Toolchain & Prerequisite Setup

The ESP32-C6 is a RISC-V microcontroller. While standard Rust targets it via `riscv32imac-unknown-none-elf`, building the embedded OpenThread stack requires compiling C sources and generating bindings.

### Installation Steps

1. **Install standard Rust components:**
   Ensure you have the RISC-V target added:
   ```bash
   rustup target add riscv32imac-unknown-none-elf
   rustup component add rust-src
   ```

2. **Install `espup`:**
   `espup` is not distributed via standard Homebrew core. The official and recommended way to install it is via Cargo. You can build it from source or install pre-compiled binaries using `cargo-binstall` to save time:
   ```bash
   # Option A: Install via Cargo (compiles from source)
   cargo install espup --locked

   # Option B: Quick install (uses pre-compiled binaries if cargo-binstall is installed)
   cargo binstall espup
   
   # Run the installation
   espup install
   ```

3. **Source the Environment Variables:**
   After installation, you must load the Espressif environment variables into your shell. Add this to your shell profile (`.zshrc` or `.bashrc`) or run it before building:
   ```bash
   . $HOME/export-esp.sh
   ```
   *Note: This ensures the build script can resolve `riscv32-esp-elf-gcc` and libclang.*

4. **Install Host Prerequisites (LLVM/Clang):**
   If `bindgen` complains about missing LLVM/libclang headers, install it via Homebrew (macOS) or your package manager:
   ```bash
   brew install llvm
   ```

---

## 2. Pinned Dependencies & Forking `rs-matter`

To adapt the protocol behavior to the Thread-only ESP32-C6 without Wi-Fi capabilities, we patched the `rs-matter` crate.

### Forking the Library
We forked the upstream `sysgrok/rs-matter` repository to map local fixes to a persistent, remote Git repository:
1. Forked via the GitHub CLI:
   ```bash
   cd rs-matter
   gh repo fork --remote
   ```
2. Created a dedicated branch:
   ```bash
   git checkout -b esp32c6-thread-fixes
   ```
3. Pushed fixes to the fork:
   ```bash
   git push -u origin esp32c6-thread-fixes
   ```

### Cargo.toml Patching
The workspace `Cargo.toml` is configured to override the crates.io version of `rs-matter` with our patched fork on GitHub:
```toml
[patch.crates-io]
rs-matter = { git = "https://github.com/melastmohican/rs-matter.git", branch = "esp32c6-thread-fixes" }
```

---

## 3. Applied Code Modifications & Rationale

### Fix 1: Network Commissioning Cluster Type Parameterization
* **Files:** 
  * `rs-matter/src/dm/clusters/net_comm.rs`
  * `rs-matter/src/dm/endpoints.rs`
* **Change:** Parameterized `NetCommHandler` to support generic network types (`EthernetType`, `WifiType`, `ThreadType`). Adjusted the handlers (`eth_sys_handler`, `wifi_sys_handler`, `thread_sys_handler`) to instantiate the correct variant.
* **Rationale:** In standard `rs-matter`, the Network Commissioning cluster ID was hardcoded to Ethernet. However, Thread devices must advertise the Thread Network Commissioning cluster ID (`0x0035`), otherwise Matter commissioners will fail to find or configure the network interface.

### Fix 2: Stubbing the Wireless Network Scan
* **File:** `rs-matter/src/dm/networks/wireless.rs`
* **Change:** Changed `NoopWirelessNetCtl::scan` to return `Ok(())` instead of `Err(...)`.
* **Rationale:** During BLE-assisted commissioning, Home Assistant queries the device to perform a Wi-Fi network scan. Since this is a Thread-only stack, Wi-Fi is stubbed out with `NoopWirelessNetCtl`. Returning `NotImplemented` caused the controller to immediately abort commissioning and loop indefinitely. Returning a dummy success (`Ok(())`) satisfies the query and lets commissioning proceed over Thread.

### Fix 3: Log Severity Demotion for Speculative Queries
* **File:** `rs-matter/src/dm/types/reply.rs`
* **Change:** Changed `error!` to `debug!` on line 129 inside `do_process_read`'s `Err` handler. Also improved command invocation error logging.
* **Rationale:** Matter controllers routinely probe the device for unsupported/optional features (like sleepy-device management or Ethernet diagnostics). Returning `UnsupportedCluster` or `UnsupportedAttribute` is expected protocol behavior, but `rs-matter` originally printed them as hard `[ERROR]` warnings. Demoting these logs to `debug!` prevents spamming the console.

### Fix 4: Main Example Setup
* **File:** `examples/matter_thread_light.rs`
* **Change:** Switched the main logic runner to `stack.run_coex(...)` and configured the network stack properties.
* **Rationale:** Concurrent commissioning mode ensures that both BLE (for onboarding pairing) and Thread (for active operation) are active at the same time, allowing Home Assistant to detect and configure the device over BLE before transitioning it to the Thread network.

---

## 4. Run/Verify

Run the example on the connected ESP32-C6 board:
```bash
cargo run --example matter_thread_light
```

> [!IMPORTANT]
> **Reprovisioning Requirement:** This example is configured to use a temporary, RAM-only Key-Value store (`DummyKvBlobStore`) to hold its Matter operational credentials. Because credentials are not persisted to Non-Volatile Storage (Flash), the device will completely reset its pairing state on every restart or reflash. You must remove the previous device instance from Home Assistant and pair it as a new device every time you re-run the example.

Verify that the output generates the QR code and connects successfully to the Thread Border Router once commissioned.
