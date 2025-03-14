mod store;

use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use log::debug;
use milli::heed::{Env, RwTxn};
use time::OffsetDateTime;

use super::batch::BatchContent;
use super::error::TaskError;
use super::scheduler::Processing;
use super::task::{Task, TaskContent, TaskId};
use super::Result;
use crate::tasks::task::TaskEvent;
use crate::update_file_store::UpdateFileStore;

#[cfg(test)]
pub use store::test::MockStore as Store;
#[cfg(not(test))]
pub use store::Store;

/// Defines constraints to be applied when querying for Tasks from the store.
#[derive(Default)]
pub struct TaskFilter {
    indexes: Option<HashSet<String>>,
    filter_fn: Option<Box<dyn Fn(&Task) -> bool + Sync + Send + 'static>>,
}

impl TaskFilter {
    fn pass(&self, task: &Task) -> bool {
        match task.index_uid() {
            Some(index_uid) => self
                .indexes
                .as_ref()
                .map_or(true, |indexes| indexes.contains(index_uid)),
            None => false,
        }
    }

    fn filtered_indexes(&self) -> Option<&HashSet<String>> {
        self.indexes.as_ref()
    }

    /// Adds an index to the filter, so the filter must match this index.
    pub fn filter_index(&mut self, index: String) {
        self.indexes
            .get_or_insert_with(Default::default)
            .insert(index);
    }

    pub fn filter_fn(&mut self, f: impl Fn(&Task) -> bool + Sync + Send + 'static) {
        self.filter_fn.replace(Box::new(f));
    }
}

pub struct TaskStore {
    store: Arc<Store>,
}

impl Clone for TaskStore {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
        }
    }
}

impl TaskStore {
    pub fn new(env: Arc<milli::heed::Env>) -> Result<Self> {
        let store = Arc::new(Store::new(env)?);
        Ok(Self { store })
    }

    pub async fn register(&self, content: TaskContent) -> Result<Task> {
        debug!("registering update: {:?}", content);
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || -> Result<Task> {
            let mut txn = store.wtxn()?;
            let next_task_id = store.next_task_id(&mut txn)?;
            let created_at = TaskEvent::Created(OffsetDateTime::now_utc());
            let task = Task {
                id: next_task_id,
                content,
                events: vec![created_at],
            };

            store.put(&mut txn, &task)?;
            txn.commit()?;

            Ok(task)
        })
        .await??;

        Ok(task)
    }

    pub fn register_raw_update(&self, wtxn: &mut RwTxn, task: &Task) -> Result<()> {
        self.store.put(wtxn, task)?;
        Ok(())
    }

    pub async fn get_task(&self, id: TaskId, filter: Option<TaskFilter>) -> Result<Task> {
        let store = self.store.clone();
        let task = tokio::task::spawn_blocking(move || -> Result<_> {
            let txn = store.rtxn()?;
            let task = store.get(&txn, id)?;
            Ok(task)
        })
        .await??
        .ok_or(TaskError::UnexistingTask(id))?;

        match filter {
            Some(filter) => filter
                .pass(&task)
                .then(|| task)
                .ok_or(TaskError::UnexistingTask(id)),
            None => Ok(task),
        }
    }

    /// This methods takes a `Processing` which contains the next task ids to process, and returns
    /// the corresponding tasks along with the ownership to the passed processing.
    ///
    /// We need get_processing_tasks to take ownership over `Processing` because we need it to be
    /// valid for 'static.
    pub async fn get_processing_tasks(
        &self,
        processing: Processing,
    ) -> Result<(Processing, BatchContent)> {
        let store = self.store.clone();
        let tasks = tokio::task::spawn_blocking(move || -> Result<_> {
            let txn = store.rtxn()?;

            let content = match processing {
                Processing::DocumentAdditions(ref ids) => {
                    let mut tasks = Vec::new();

                    for id in ids.iter() {
                        let task = store
                            .get(&txn, *id)?
                            .ok_or(TaskError::UnexistingTask(*id))?;
                        tasks.push(task);
                    }
                    BatchContent::DocumentsAdditionBatch(tasks)
                }
                Processing::IndexUpdate(id) => {
                    let task = store.get(&txn, id)?.ok_or(TaskError::UnexistingTask(id))?;
                    BatchContent::IndexUpdate(task)
                }
                Processing::Dump(id) => {
                    let task = store.get(&txn, id)?.ok_or(TaskError::UnexistingTask(id))?;
                    debug_assert!(matches!(task.content, TaskContent::Dump { .. }));
                    BatchContent::Dump(task)
                }
                Processing::Nothing => BatchContent::Empty,
            };

            Ok((processing, content))
        })
        .await??;

        Ok(tasks)
    }

    pub async fn update_tasks(&self, tasks: Vec<Task>) -> Result<Vec<Task>> {
        let store = self.store.clone();

        let tasks = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut txn = store.wtxn()?;

            for task in &tasks {
                store.put(&mut txn, task)?;
            }

            txn.commit()?;

            Ok(tasks)
        })
        .await??;

        Ok(tasks)
    }

    pub async fn fetch_unfinished_tasks(&self, offset: Option<TaskId>) -> Result<Vec<Task>> {
        let store = self.store.clone();

        tokio::task::spawn_blocking(move || {
            let txn = store.rtxn()?;
            let tasks = store.fetch_unfinished_tasks(&txn, offset)?;
            Ok(tasks)
        })
        .await?
    }

    pub async fn list_tasks(
        &self,
        offset: Option<TaskId>,
        filter: Option<TaskFilter>,
        limit: Option<usize>,
    ) -> Result<Vec<Task>> {
        let store = self.store.clone();

        tokio::task::spawn_blocking(move || {
            let txn = store.rtxn()?;
            let tasks = store.list_tasks(&txn, offset, filter, limit)?;
            Ok(tasks)
        })
        .await?
    }

    pub async fn dump(
        env: Arc<Env>,
        dir_path: impl AsRef<Path>,
        update_file_store: UpdateFileStore,
    ) -> Result<()> {
        let store = Self::new(env)?;
        let update_dir = dir_path.as_ref().join("updates");
        let updates_file = update_dir.join("data.jsonl");
        let tasks = store.list_tasks(None, None, None).await?;

        let dir_path = dir_path.as_ref().to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<()> {
            std::fs::create_dir(&update_dir)?;
            let updates_file = std::fs::File::create(updates_file)?;
            let mut updates_file = BufWriter::new(updates_file);

            for task in tasks {
                serde_json::to_writer(&mut updates_file, &task)?;
                updates_file.write_all(b"\n")?;

                if !task.is_finished() {
                    if let Some(content_uuid) = task.get_content_uuid() {
                        update_file_store.dump(content_uuid, &dir_path)?;
                    }
                }
            }
            updates_file.flush()?;
            Ok(())
        })
        .await??;

        Ok(())
    }

    pub fn load_dump(src: impl AsRef<Path>, env: Arc<Env>) -> anyhow::Result<()> {
        // create a dummy update field store, since it is not needed right now.
        let store = Self::new(env.clone())?;

        let src_update_path = src.as_ref().join("updates");
        let update_data = std::fs::File::open(&src_update_path.join("data.jsonl"))?;
        let update_data = std::io::BufReader::new(update_data);

        let stream = serde_json::Deserializer::from_reader(update_data).into_iter::<Task>();

        let mut wtxn = env.write_txn()?;
        for entry in stream {
            store.register_raw_update(&mut wtxn, &entry?)?;
        }
        wtxn.commit()?;

        Ok(())
    }
}

#[cfg(test)]
pub mod test {
    use crate::tasks::{scheduler::Processing, task_store::store::test::tmp_env};

    use super::*;

    use meilisearch_types::index_uid::IndexUid;
    use nelson::Mocker;
    use proptest::{
        strategy::Strategy,
        test_runner::{Config, TestRunner},
    };

    pub enum MockTaskStore {
        Real(TaskStore),
        Mock(Arc<Mocker>),
    }

    impl Clone for MockTaskStore {
        fn clone(&self) -> Self {
            match self {
                Self::Real(x) => Self::Real(x.clone()),
                Self::Mock(x) => Self::Mock(x.clone()),
            }
        }
    }

    impl MockTaskStore {
        pub fn new(env: Arc<milli::heed::Env>) -> Result<Self> {
            Ok(Self::Real(TaskStore::new(env)?))
        }

        pub async fn dump(
            env: Arc<milli::heed::Env>,
            path: impl AsRef<Path>,
            update_file_store: UpdateFileStore,
        ) -> Result<()> {
            TaskStore::dump(env, path, update_file_store).await
        }

        pub fn mock(mocker: Mocker) -> Self {
            Self::Mock(Arc::new(mocker))
        }

        pub async fn update_tasks(&self, tasks: Vec<Task>) -> Result<Vec<Task>> {
            match self {
                Self::Real(s) => s.update_tasks(tasks).await,
                Self::Mock(m) => unsafe {
                    m.get::<_, Result<Vec<Task>>>("update_tasks").call(tasks)
                },
            }
        }

        pub async fn get_task(&self, id: TaskId, filter: Option<TaskFilter>) -> Result<Task> {
            match self {
                Self::Real(s) => s.get_task(id, filter).await,
                Self::Mock(m) => unsafe { m.get::<_, Result<Task>>("get_task").call((id, filter)) },
            }
        }

        pub async fn get_processing_tasks(
            &self,
            tasks: Processing,
        ) -> Result<(Processing, BatchContent)> {
            match self {
                Self::Real(s) => s.get_processing_tasks(tasks).await,
                Self::Mock(m) => unsafe { m.get("get_pending_task").call(tasks) },
            }
        }

        pub async fn fetch_unfinished_tasks(&self, from: Option<TaskId>) -> Result<Vec<Task>> {
            match self {
                Self::Real(s) => s.fetch_unfinished_tasks(from).await,
                Self::Mock(m) => unsafe { m.get("fetch_unfinished_tasks").call(from) },
            }
        }

        pub async fn list_tasks(
            &self,
            from: Option<TaskId>,
            filter: Option<TaskFilter>,
            limit: Option<usize>,
        ) -> Result<Vec<Task>> {
            match self {
                Self::Real(s) => s.list_tasks(from, filter, limit).await,
                Self::Mock(m) => unsafe { m.get("list_tasks").call((from, filter, limit)) },
            }
        }

        pub async fn register(&self, content: TaskContent) -> Result<Task> {
            match self {
                Self::Real(s) => s.register(content).await,
                Self::Mock(_m) => todo!(),
            }
        }

        pub fn register_raw_update(&self, wtxn: &mut RwTxn, task: &Task) -> Result<()> {
            match self {
                Self::Real(s) => s.register_raw_update(wtxn, task),
                Self::Mock(_m) => todo!(),
            }
        }

        pub fn load_dump(path: impl AsRef<Path>, env: Arc<Env>) -> anyhow::Result<()> {
            TaskStore::load_dump(path, env)
        }
    }

    #[test]
    fn test_increment_task_id() {
        let tmp = tmp_env();
        let store = Store::new(tmp.env()).unwrap();

        let mut txn = store.wtxn().unwrap();
        assert_eq!(store.next_task_id(&mut txn).unwrap(), 0);
        txn.abort().unwrap();

        let gen_task = |id: TaskId| Task {
            id,
            content: TaskContent::IndexCreation {
                primary_key: None,
                index_uid: IndexUid::new_unchecked("test"),
            },
            events: Vec::new(),
        };

        let mut runner = TestRunner::new(Config::default());
        runner
            .run(&(0..100u32).prop_map(gen_task), |task| {
                let mut txn = store.wtxn().unwrap();
                let previous_id = store.next_task_id(&mut txn).unwrap();

                store.put(&mut txn, &task).unwrap();

                let next_id = store.next_task_id(&mut txn).unwrap();

                // if we put a task whose task_id is less than the next_id, then the next_id remains
                // unchanged, otherwise it becomes task.id + 1
                if task.id < previous_id {
                    assert_eq!(next_id, previous_id)
                } else {
                    assert_eq!(next_id, task.id + 1);
                }

                txn.commit().unwrap();

                Ok(())
            })
            .unwrap();
    }
}
