//! How the newest request of each list ended (issue #297), kept in the store because the client
//! that runs requests and the UI adapter that reads the status only share the store. A device that
//! restarts offline knows at once that it could not read a list, without waiting for a deadline.

use serde::{Deserialize, Serialize};
use teamcraft_client_api::list_load::{ListLoad, LoadFailure, LoadingList};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

const LIST_OUTCOMES_KEY: &str = "list_outcomes";
/// Lists remembered; directory searches are the many ones. Beyond this the oldest go first and
/// count as not answered, so such a list asks again.
const MAX_OUTCOMES: usize = 64;

/// The outcomes, oldest first.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ListOutcomes {
  entries: Vec<ListOutcomeEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ListOutcomeEntry {
  list: LoadingList,
  load: ListLoad,
}

/// Remembers how a list's request ended: `Ready` or `Failed`.
pub(crate) fn note(store: &impl LocalStore, list: LoadingList, load: ListLoad) -> Result<(), CoreError> {
  let mut outcomes = read(store)?;
  outcomes.entries.retain(|entry| entry.list != list);
  outcomes.entries.push(ListOutcomeEntry { list, load });
  let excess = outcomes.entries.len().saturating_sub(MAX_OUTCOMES);
  outcomes.entries.drain(..excess);
  records::put_json(store, Table::SyncState, LIST_OUTCOMES_KEY, &outcomes)
}

/// The status a list's newest request left: `Loading` while none has ended.
pub(crate) fn load_of(store: &impl LocalStore, list: &LoadingList) -> Result<ListLoad, CoreError> {
  let outcomes = read(store)?;
  let maybe_entry = outcomes.entries.iter().find(|entry| entry.list == *list);
  Ok(maybe_entry.map_or(ListLoad::Loading, |entry| entry.load))
}

/// The user retries: the list is loading again until the new request ends.
pub(crate) fn forget(store: &impl LocalStore, list: &LoadingList) -> Result<(), CoreError> {
  let mut outcomes = read(store)?;
  outcomes.entries.retain(|entry| entry.list != *list);
  records::put_json(store, Table::SyncState, LIST_OUTCOMES_KEY, &outcomes)
}

pub(crate) fn failed(error: &CoreError) -> ListLoad {
  ListLoad::Failed {
    failure: LoadFailure::from(&error.to_failure().kind),
  }
}

fn read(store: &impl LocalStore) -> Result<ListOutcomes, CoreError> {
  let maybe_outcomes = records::get_json(store, Table::SyncState, LIST_OUTCOMES_KEY)?;
  Ok(maybe_outcomes.unwrap_or_default())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::MemoryStore;

  const FAILED: ListLoad = ListLoad::Failed {
    failure: LoadFailure::Network,
  };

  #[test]
  fn a_list_loads_until_its_request_ended_and_again_after_a_retry() {
    let store = MemoryStore::new();
    let list = LoadingList::CallHistory;
    assert_eq!(load_of(&store, &list).unwrap(), ListLoad::Loading);
    note(&store, list.clone(), FAILED).unwrap();
    assert_eq!(load_of(&store, &list).unwrap(), FAILED);
    forget(&store, &list).unwrap();
    assert_eq!(load_of(&store, &list).unwrap(), ListLoad::Loading);
    note(&store, list.clone(), ListLoad::Ready).unwrap();
    assert_eq!(load_of(&store, &list).unwrap(), ListLoad::Ready);
  }

  #[test]
  fn the_outcomes_stay_bounded_and_the_oldest_go_first() {
    let store = MemoryStore::new();
    for number in 0..=MAX_OUTCOMES {
      note(
        &store,
        LoadingList::directory(&number.to_string()),
        ListLoad::Ready,
      )
      .unwrap();
    }
    assert_eq!(read(&store).unwrap().entries.len(), MAX_OUTCOMES);
    let oldest = LoadingList::directory("0");
    assert_eq!(load_of(&store, &oldest).unwrap(), ListLoad::Loading);
    let newest = LoadingList::directory(&MAX_OUTCOMES.to_string());
    assert_eq!(load_of(&store, &newest).unwrap(), ListLoad::Ready);
  }
}
