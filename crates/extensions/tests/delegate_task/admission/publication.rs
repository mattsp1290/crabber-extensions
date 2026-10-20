//! Synthetic host isolates permit lifetime at publication; actual runtime joins
//! are qualified by detached/attribution tests rather than inferred here.
use super::*;
use crabber::{
    core::{ControlId, PublicSourceChange, PublicStateContents, SessionRevision},
    extension::{PublicSourceError, PublicSourceSink, PublicSourceSnapshot, PublicSourceWriter},
};
use std::sync::atomic::AtomicBool;
use tokio::sync::Semaphore;

struct PublishingHost {
    first_terminal: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}
#[async_trait::async_trait]
impl PublicSourceSink for PublishingHost {
    async fn snapshot(&self) -> Result<PublicSourceSnapshot, PublicSourceError> {
        Ok(PublicSourceSnapshot {
            revision: SessionRevision::INITIAL,
            contents: PublicStateContents::default(),
        })
    }
    async fn publish(
        &self,
        _: ControlId,
        revision: SessionRevision,
        change: PublicSourceChange,
    ) -> Result<SessionRevision, PublicSourceError> {
        if matches!(change, PublicSourceChange::ActivityDelta { .. })
            && self.first_terminal.swap(false, Ordering::SeqCst)
        {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        Ok(revision)
    }
}
struct JoinedPublisher(Arc<PublishingHost>);
#[async_trait::async_trait]
impl SessionInvocationSpawner for JoinedPublisher {
    async fn spawn(
        &self,
        _: SessionInvocationRequest,
        task: SessionInvocationTask,
    ) -> Result<Admission, Error> {
        let mut context = invocation();
        context.public_source = PublicSourceWriter::from_host(self.0.clone());
        task(context).await.unwrap();
        Ok(Admission::Started)
    }
}

#[tokio::test]
async fn final_publication_holds_capacity_and_completion_or_drop_releases_it() {
    for abort in [false, true] {
        let ext = Arc::new(configured(
            runner(Response::Completed("retained".into())),
            Limits {
                max_in_flight: 1,
                ..limits()
            },
        ));
        let (_, handle, tools) = mounted(ext.clone()).await;
        let publication = Arc::new(PublishingHost {
            first_terminal: AtomicBool::new(true),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        let context = context(CancellationToken::new()).with_session_invocations(
            SessionInvocations::from_host(Arc::new(JoinedPublisher(publication.clone()))),
        );
        let start = tools[START_TOOL].clone();
        let original = context.clone();
        let call =
            tokio::spawn(
                async move { start.execute_json_with_context(original, arguments()).await },
            );
        tokio::time::timeout(Duration::from_secs(3), publication.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
        assert_eq!(ext.live_runners(), 1, "publication still owns capacity");
        assert_eq!(
            tools[START_TOOL]
                .execute_json_with_context(next(context.clone()), arguments())
                .await
                .unwrap()["status"],
            "unavailable"
        );
        if abort {
            call.abort();
            assert!(call.await.unwrap_err().is_cancelled());
        } else {
            publication.release.add_permits(1);
            assert_eq!(call.await.unwrap().unwrap()["status"], "started");
        }
        assert_eq!(ext.live_runners(), 0);
        assert_eq!(
            tools[START_TOOL]
                .execute_json_with_context(next(context), arguments())
                .await
                .unwrap()["status"],
            "started",
            "capacity was released after the lifecycle ended"
        );
        handle.close().await.unwrap();
    }
}
