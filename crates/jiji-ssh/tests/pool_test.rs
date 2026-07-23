use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use jiji_ssh::SshPool;

#[tokio::test]
async fn never_exceeds_max_concurrent_operations() {
    let pool = SshPool::new(3);
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_observed = Arc::new(AtomicUsize::new(0));

    let operations: Vec<_> = (0..10)
        .map(|_| {
            let in_flight = Arc::clone(&in_flight);
            let max_observed = Arc::clone(&max_observed);
            move || async move {
                let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_observed.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(20)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                current
            }
        })
        .collect();

    let results = pool.execute_concurrent(operations).await;

    assert_eq!(results.len(), 10);
    assert!(max_observed.load(Ordering::SeqCst) <= 3);
}

#[tokio::test]
async fn preserves_result_order() {
    let pool = SshPool::new(4);

    let operations: Vec<_> = (0..8)
        .map(|i| {
            move || async move {
                // Later operations sleep less, so if ordering weren't preserved they'd finish
                // (and get pushed) out of order.
                tokio::time::sleep(Duration::from_millis((8 - i) * 5)).await;
                i
            }
        })
        .collect();

    let results = pool.execute_concurrent(operations).await;
    assert_eq!(results, (0..8).collect::<Vec<_>>());
}

#[tokio::test]
async fn batches_run_sequentially_but_concurrently_within_a_batch() {
    let pool = SshPool::new(10);
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_observed = Arc::new(AtomicUsize::new(0));

    let operations: Vec<_> = (0..6)
        .map(|i| {
            let in_flight = Arc::clone(&in_flight);
            let max_observed = Arc::clone(&max_observed);
            move || async move {
                let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_observed.fetch_max(current, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(15)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                i
            }
        })
        .collect();

    let results = pool.execute_batched(operations, Some(2)).await;

    assert_eq!(results, (0..6).collect::<Vec<_>>());
    // Batches of 2 should let at least 2 run concurrently, but never more than the batch size.
    assert!(max_observed.load(Ordering::SeqCst) <= 2);
    assert!(max_observed.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn error_collection_partitions_ok_and_err() {
    let pool = SshPool::new(4);

    let operations: Vec<_> = [1, 2, 3]
        .into_iter()
        .map(|i| {
            move || async move {
                if i == 2 {
                    Err::<i32, &'static str>("boom")
                } else {
                    Ok::<i32, &'static str>(i)
                }
            }
        })
        .collect();

    let (results, errors) = pool.execute_with_error_collection(operations).await;

    assert_eq!(results, vec![1, 3]);
    assert_eq!(errors, vec!["boom"]);
}
