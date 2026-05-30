# KontsnorOS Driver SDK

The official SDK for developing hardware drivers for KontsnorOS.

## Features

- **Safe by default** — Use safe Rust for most driver logic
- **Stable API** — Versioned trait interface with backward compatibility
- **Comprehensive types** — DMA buffers, MMIO regions, IRQ handlers
- **Permissive license** — MIT/Apache-2.0 enables proprietary drivers
- **Well documented** — Every API has documentation and examples

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
kontsnor-driver-sdk = "0.1"
```

See [docs/driver-guide.md](../docs/driver-guide.md) for the full development guide.

## License

Licensed under either MIT or Apache-2.0 at your option.
