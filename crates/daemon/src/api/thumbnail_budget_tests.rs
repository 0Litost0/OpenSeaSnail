use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

use super::sessions::acquire_thumbnail_permits;

#[tokio::test]
async fn same_session_thumbnail_waits_and_does_not_starve_another_session() {
    let global = Arc::new(Semaphore::new(2));
    let session = Arc::new(Semaphore::new(1));
    let first = acquire_thumbnail_permits(session.clone(), global.clone(), Duration::from_secs(1))
        .await
        .unwrap();
    let mut second = Box::pin(acquire_thumbnail_permits(
        session.clone(),
        global.clone(),
        Duration::from_secs(1),
    ));
    // Poll while the first image holds its permits. The second must wait, not
    // fail immediately or reserve the remaining global slot.
    tokio::select! {
        biased;
        result = &mut second => panic!("second thumbnail did not wait: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }
    assert_eq!(global.available_permits(), 1);
    let other = acquire_thumbnail_permits(
        Arc::new(Semaphore::new(1)),
        global.clone(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    drop(other);
    drop(first);
    let second = tokio::time::timeout(Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap();
    drop(second);
    assert_eq!(global.available_permits(), 2);
    assert_eq!(session.available_permits(), 1);
}

#[tokio::test]
async fn thumbnail_wait_timeout_releases_session_capacity() {
    let global = Arc::new(Semaphore::new(1));
    let occupied = global.clone().acquire_owned().await.unwrap();
    let session = Arc::new(Semaphore::new(1));
    let result =
        acquire_thumbnail_permits(session.clone(), global.clone(), Duration::from_millis(10)).await;
    assert!(matches!(
        result,
        Err(crate::error::AppError::ThumbnailBusy(_))
    ));
    assert_eq!(session.available_permits(), 1);
    drop(occupied);
    assert!(
        acquire_thumbnail_permits(session, global, Duration::from_secs(1))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn cancelling_a_queued_thumbnail_releases_its_permits() {
    let global = Arc::new(Semaphore::new(1));
    let occupied = global.clone().acquire_owned().await.unwrap();
    let session = Arc::new(Semaphore::new(1));
    let mut pending = Box::pin(acquire_thumbnail_permits(
        session.clone(),
        global.clone(),
        Duration::from_secs(1),
    ));
    tokio::select! {
        biased;
        result = &mut pending => panic!("thumbnail did not wait: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }
    assert_eq!(session.available_permits(), 0);
    drop(pending);
    assert_eq!(session.available_permits(), 1);
    drop(occupied);
    assert_eq!(global.available_permits(), 1);
}

#[tokio::test]
async fn global_and_session_thumbnail_limits_are_independent() {
    let global = Arc::new(Semaphore::new(2));
    let session_a = Arc::new(Semaphore::new(1));
    let session_b = Arc::new(Semaphore::new(1));

    let global_a = global.clone().try_acquire_owned().unwrap();
    let global_b = global.clone().try_acquire_owned().unwrap();
    assert!(global.clone().try_acquire_owned().is_err());

    let session_a_permit = session_a.clone().try_acquire_owned().unwrap();
    assert!(session_a.clone().try_acquire_owned().is_err());
    let session_b_permit = session_b.clone().try_acquire_owned().unwrap();

    drop(global_a);
    assert!(global.clone().try_acquire_owned().is_ok());
    drop(global_b);
    drop(session_a_permit);
    drop(session_b_permit);
    assert!(session_a.try_acquire().is_ok());
    assert!(session_b.try_acquire().is_ok());
}
