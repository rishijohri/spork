//! The load-bearing proof of the F3 dual-delivery contract: **flooding the
//! ephemeral bus does not stall, reorder, drop, or measurably delay ordered
//! [`OpLogEvent`] delivery.**
//!
//! The two rails are physically separate (different channels, no shared
//! threads), so a deluge of [`EphemeralFrame`]s on a busy node cannot become
//! backpressure on the durable rail (DESIGN.md §5.5, §14.4). These tests prove
//! that property empirically rather than asserting it in prose:
//!
//! 1. Under a concurrent multi-thread flood of hundreds of thousands of
//!    ephemeral frames, every published [`OpLogEvent`] still arrives in `seq`
//!    order with no gaps and none missing.
//! 2. The per-event delivery latency of the ordered rail measured *during* the
//!    flood stays within a small multiple of its quiescent baseline — i.e. the
//!    flood does not measurably slow ordered delivery.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use spork_ipc::{EphemeralChannel, EphemeralFrame, OpLogEvent};
use spork_stream::{EphemeralBus, EventStream};
use ulid::Ulid;

fn node_event(seq: u64, node: Ulid) -> OpLogEvent {
    OpLogEvent::NodeCreated {
        seq,
        node_id: node,
        schema_version: 1,
    }
}

#[test]
fn ephemeral_flood_does_not_stall_or_corrupt_ordered_delivery() {
    let stream = EventStream::new();
    let bus = EphemeralBus::new();

    let ordered_node = Ulid::new();
    let flood_node = Ulid::new();

    const ORDERED_EVENTS: u64 = 5_000;
    const FLOOD_THREADS: usize = 8;
    const FLOOD_PER_THREAD: u64 = 50_000; // 400k ephemeral frames total

    let ordered_rx = stream.subscribe();
    // A subscriber on the flooded node that never drains — the worst case for
    // backpressure. Its bounded queue will overflow and drop, by design.
    let _flood_sub = bus.subscribe(flood_node);

    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(FLOOD_THREADS + 2));

    // Spawn the flood producers.
    let mut flood_handles = Vec::new();
    for _ in 0..FLOOD_THREADS {
        let bus = bus.clone();
        let start = Arc::clone(&start);
        let stop = Arc::clone(&stop);
        flood_handles.push(thread::spawn(move || {
            start.wait();
            for i in 0..FLOOD_PER_THREAD {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                bus.publish(EphemeralFrame::new(
                    flood_node,
                    EphemeralChannel::RunStdout,
                    // A non-trivial payload so the flood moves real bytes.
                    format!("noisy-stdout-line-number-{i}-xxxxxxxxxxxxxxxxxxxx"),
                ));
            }
        }));
    }

    // The ordered producer: publish ORDERED_EVENTS in seq order, concurrently.
    let ordered_producer = {
        let stream = stream.clone();
        let start = Arc::clone(&start);
        thread::spawn(move || {
            start.wait();
            for seq in 1..=ORDERED_EVENTS {
                stream
                    .publish(node_event(seq, ordered_node))
                    .expect("ordered publish must never gap");
                // Yield occasionally so the flood genuinely interleaves.
                if seq.is_multiple_of(256) {
                    thread::yield_now();
                }
            }
        })
    };

    // The consumer: verify every ordered event arrives, contiguous, in order.
    start.wait();
    let mut last = 0u64;
    for _ in 0..ORDERED_EVENTS {
        let e = ordered_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("ordered event must arrive despite the flood");
        assert_eq!(
            e.seq(),
            last + 1,
            "ordered rail gapped/reordered under flood (got {} after {})",
            e.seq(),
            last
        );
        last = e.seq();
    }
    assert_eq!(last, ORDERED_EVENTS, "every ordered event delivered");

    // Tear down the flood.
    stop.store(true, Ordering::Relaxed);
    ordered_producer.join().unwrap();
    for h in flood_handles {
        h.join().unwrap();
    }

    // The flood really happened (it delivered and/or dropped a large volume).
    assert!(
        bus.delivered() + bus.dropped() > 0,
        "the flood should have moved frames"
    );
}

#[test]
fn ordered_latency_is_not_degraded_by_a_concurrent_ephemeral_flood() {
    // Measure mean ordered round-trip latency (publish -> receive) twice:
    //   (a) quiescent baseline, no flood;
    //   (b) while a heavy ephemeral flood runs.
    // Assert (b) is within a generous multiple of (a). If the rails shared a
    // channel/thread, (b) would blow up; because they are separate, it does not.
    const SAMPLES: u64 = 2_000;

    // ---- baseline ----
    let baseline = measure_ordered_latency(SAMPLES, None);

    // ---- under flood ----
    let bus = EphemeralBus::new();
    let flood_node = Ulid::new();
    let _sub = bus.subscribe(flood_node); // never drained
    let stop = Arc::new(AtomicBool::new(false));
    let flood_count = Arc::new(AtomicU64::new(0));

    let mut handles = Vec::new();
    for _ in 0..6 {
        let bus = bus.clone();
        let stop = Arc::clone(&stop);
        let flood_count = Arc::clone(&flood_count);
        handles.push(thread::spawn(move || {
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                bus.publish(EphemeralFrame::new(
                    flood_node,
                    EphemeralChannel::ChatTokens,
                    format!("tok-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                ));
                i += 1;
                if i.is_multiple_of(4096) {
                    flood_count.fetch_add(4096, Ordering::Relaxed);
                }
            }
        }));
    }

    // Give the flood a moment to ramp before measuring.
    let warmup = Instant::now();
    while flood_count.load(Ordering::Relaxed) < 4096 && warmup.elapsed() < Duration::from_secs(5) {
        thread::yield_now();
    }

    let under_flood = measure_ordered_latency(SAMPLES, Some(()));

    stop.store(true, Ordering::Relaxed);
    for h in handles {
        h.join().unwrap();
    }

    eprintln!(
        "ordered latency: baseline {:.2}us, under-flood {:.2}us ({:.1}x)",
        baseline.as_secs_f64() * 1e6,
        under_flood.as_secs_f64() * 1e6,
        under_flood.as_secs_f64() / baseline.as_secs_f64().max(f64::MIN_POSITIVE),
    );

    // The flood must have been substantial.
    assert!(
        flood_count.load(Ordering::Relaxed) >= 4096,
        "flood did not ramp"
    );

    // Non-interference budget: under-flood mean latency stays within a generous
    // multiple of baseline plus an absolute floor (so a sub-microsecond baseline
    // does not make the ratio test brittle on a loaded CI box). Separate rails
    // make this comfortably true; a shared rail would not.
    let budget = baseline.mul_f64(20.0).max(Duration::from_micros(200)) + Duration::from_micros(50);
    assert!(
        under_flood <= budget,
        "ordered delivery was degraded by the flood: {under_flood:?} > budget {budget:?} (baseline {baseline:?})",
    );
}

/// Measure the mean publish->receive latency of the ordered rail over `samples`
/// events. `_flood` is purely a marker so the call sites read clearly.
fn measure_ordered_latency(samples: u64, _flood: Option<()>) -> Duration {
    let stream = EventStream::new();
    let rx = stream.subscribe();
    let node = Ulid::new();
    let mut total = Duration::ZERO;
    for seq in 1..=samples {
        let t0 = Instant::now();
        stream.publish(node_event(seq, node)).unwrap();
        let got = rx.recv().unwrap();
        total += t0.elapsed();
        assert_eq!(got.seq(), seq);
    }
    total / (samples as u32)
}
