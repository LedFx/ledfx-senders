# Native benchmark receiver

This is a loopback-only measurement tool, separate from the production sender.
Build with `cargo build --release --bin ledfx-receiver` from `native`, then run:

```
ledfx-receiver ddp fixture.rgb batched 512 1
```

Arguments are protocol (`ddp`, `e131`, `opc`), expected raw RGB bytes, receive
backend (`portable` or `batched`), bounded reorder window, and DDP destination ID
or OPC channel. E1.31 uses the benchmark's default universe 1, 510 driven slots,
zero offset, synchronization universe 63999. The ready JSON reports an ephemeral
127.0.0.1 port, actual receive buffer size, effective backend, and identity limit.
Write `stop N` to stdin with the number of submitted frames; final JSON follows.
After stopping, drain is limited to 200 ms, or 20 ms observed empty.

Each chunk starts with the low `min(8, chunk length)` bytes of a monotonically
increasing big-endian frame ID starting at 1. The rest must exactly match the
fixture. Every chunk needs at least three identity bytes; stop before the shortest
identity wraps. No protocol header is repurposed. DDP/E1.31 sequences must match
the normal initial sequence and ID progression. Identified data chunks from
separate sequence wraps cannot form a falsely complete frame. E1.31 sync packets
are validated and counted separately, since their headers cannot unambiguously
identify frames across wrap. The tool does not claim synchronized display delivery.

Frame state uses a fixed-size ring with preallocated bitmaps and 500 ms expiry.
Malformed, late, duplicate, reordered, incomplete, and entirely unseen frames
are counted separately. Completion requires every expected identified chunk.
Accepted sender datagrams must be reconciled with receiver datagrams and complete
frames by the harness; neither kernel acceptance nor a PUSH packet proves delivery.
The receiver accepts only loopback and pins the first sender address/port.

Linux batching uses preallocated `recvmmsg` storage for 64 datagrams; other
platforms use the portable socket path. CPU time comes from `getrusage` on Unix
and is null on other platforms. Receiver CPU includes its entire ready-to-stop
window; this may include sender initialization and final drain. Capacity depends
on hardware, OS buffers and scheduling. A native receiver can still bottleneck;
compare portable/batched modes, receiver CPU and observed loss under a rate ramp.

`cargo test --bin ledfx-receiver` tests the independent parser without importing
production packet encoders. After building the binary, run
`python bench/receiver/test_receiver.py` for malformed/drop/duplicate/reorder
controls over real loopback with both receive backends and all three protocols.

E131 pins the CID of the first fully validated data or control packet. Data,
synchronization, discovery and termination from another CID are invalid, even
when the UDP peer and payload identity match. Malformed data cannot pin a CID.
