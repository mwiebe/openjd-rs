// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// Copyright by contributors to this project.
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! [`ChunkTaskIndex`]: map the Tasks an Action ran to their per-Task
//! iteration indices.

use std::collections::{HashMap, VecDeque};

use openjd_expr::ExprValue;

use crate::error::ModelError;
use crate::job::step_param_space::StepParameterSpaceIterator;
use crate::job::{StepParameterSpace, TaskParameter};
use crate::types::TaskParameterSet;

/// One Task's identity: the values of its Task parameters other than the
/// `CHUNK[INT]` one, and its `CHUNK[INT]` value (`None` for a space without
/// one).
type Key = (Vec<String>, Option<i64>);

/// Maps each Task that an Action ran to its index in the Step's per-Task
/// iteration order (the order of
/// [`StepParameterSpaceIterator::new_with_chunk_override`] with
/// `Some(1)`), which is the order of a producer's `Steps.*` lists (RFC 0011
/// §Reductions items 1 and 2) and of
/// [`ProducerState::Succeeded`](crate::job::ProducerState::Succeeded).
///
/// A chunked Step runs several Tasks per Action, and the session reports
/// their outputs keyed by `CHUNK[INT]` value, not by position: a chunk's
/// values are sorted, while a list range iterates in its written order. A
/// Task is identified by the values of the Step's other Task parameters
/// (fixed within a chunk) and its `CHUNK[INT]` value. Those can repeat (a
/// `STRING` range `[a, a]`), so each key has a queue of indices, and
/// [`take`](Self::take) hands them out in order. Tasks with the same key
/// have identical parameters, so which of them gets which index is not
/// observable.
///
/// An unchunked space works the same way, with each Task keyed by all of its
/// parameter values and `None`.
///
/// The index is built lazily: [`take`](Self::take) walks the per-Task
/// iteration only as far as the Task it is asked for, so Actions that run in
/// iteration order cost O(1) amortized each, and a run that stops early
/// never walks the rest of a large space.
pub struct ChunkTaskIndex {
    chunk_param: Option<String>,
    unused: HashMap<Key, VecDeque<usize>>,
    /// The per-Task iterator, walked as far as `walked`.
    tasks: StepParameterSpaceIterator,
    walked: usize,
    task_count: usize,
}

impl std::fmt::Debug for ChunkTaskIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkTaskIndex")
            .field("chunk_param", &self.chunk_param)
            .field("walked", &self.walked)
            .field("task_count", &self.task_count)
            .finish_non_exhaustive()
    }
}

impl ChunkTaskIndex {
    /// Index the Tasks of `space`, iterated with chunking disabled. Nothing
    /// is walked yet.
    ///
    /// Errors when the space cannot be iterated (see
    /// [`StepParameterSpaceIterator::new_with_chunk_override`]).
    pub fn new(space: &StepParameterSpace) -> Result<Self, ModelError> {
        let chunk_param = space
            .task_parameter_definitions
            .iter()
            .find(|(_, p)| matches!(p, TaskParameter::ChunkInt { .. }))
            .map(|(name, _)| name.clone());
        let tasks = StepParameterSpaceIterator::new_with_chunk_override(space, Some(1))?;
        let task_count = tasks.len();
        Ok(Self {
            chunk_param,
            unused: HashMap::new(),
            tasks,
            walked: 0,
            task_count,
        })
    }

    /// The number of Tasks in the space, with chunking disabled.
    pub fn task_count(&self) -> usize {
        self.task_count
    }

    /// The name of the space's `CHUNK[INT]` parameter, if it has one.
    pub fn chunk_parameter_name(&self) -> Option<&str> {
        self.chunk_param.as_deref()
    }

    /// The next unused per-Task index of the Task with `CHUNK[INT]` value
    /// `value` in the Action whose Task parameter values are `action` (the
    /// set the iterator yielded for it, its chunk included), or `None` when
    /// no unused Task matches: `value` is not in the space, it is `Some` for
    /// a space without a `CHUNK[INT]` parameter (or `None` for one with), or
    /// every matching Task has been taken.
    pub fn take(&mut self, action: &TaskParameterSet, value: Option<i64>) -> Option<usize> {
        if value.is_some() != self.chunk_param.is_some() {
            return None;
        }
        let key = (other_values(action, self.chunk_param.as_deref()), value);
        if let Some(index) = self.unused.get_mut(&key).and_then(VecDeque::pop_front) {
            return Some(index);
        }
        // Walk on until a Task with this key turns up, queueing the others.
        for set in self.tasks.by_ref() {
            let index = self.walked;
            self.walked += 1;
            let task_value = match &self.chunk_param {
                Some(name) => set.get(name).and_then(|v| single_int(&v.value)),
                None => None,
            };
            let task_key = (other_values(&set, self.chunk_param.as_deref()), task_value);
            if task_key == key {
                return Some(index);
            }
            self.unused.entry(task_key).or_default().push_back(index);
        }
        None
    }
}

/// The values of every Task parameter of `set` but `skip`, sorted by name
/// (so a set built in another order matches), each with its type, so that
/// equal keys mean equal parameters. A `FLOAT` value is compared by number,
/// not by spelling: a range keeps its written spelling (`1.50`), while a
/// value coerced from the command line has its own.
fn other_values(set: &TaskParameterSet, skip: Option<&str>) -> Vec<String> {
    let mut values: Vec<String> = set
        .iter()
        .filter(|(name, _)| Some(name.as_str()) != skip)
        .map(|(name, v)| task_key(name, v))
        .collect();
    values.sort_unstable();
    values
}

/// One Task parameter's part of a Task's key (see [`ChunkTaskIndex`]).
pub(crate) fn task_key(name: &str, v: &crate::types::TaskParameterValue) -> String {
    let value = match &v.value {
        ExprValue::Float(f) => format!("{:#x}", (f.value() + 0.0).to_bits()),
        other => other.to_display_string(),
    };
    format!("{name}:{}:{value}", v.param_type.as_spec_str())
}

/// The one value of a single-value `CHUNK[INT]` range (or an `int`).
fn single_int(value: &ExprValue) -> Option<i64> {
    match value {
        ExprValue::Int(i) => Some(*i),
        ExprValue::RangeExpr(r) => {
            let mut it = r.iter();
            let first = it.next()?;
            it.next().is_none().then_some(first)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{ResolvedChunks, TaskParamRange};
    use crate::template::RangeConstraint;
    use indexmap::IndexMap;

    fn space(chunk: Vec<i64>, other: Option<Vec<&str>>) -> StepParameterSpace {
        let mut defs = IndexMap::new();
        defs.insert(
            "F".to_string(),
            TaskParameter::ChunkInt {
                range: TaskParamRange::List(chunk),
                chunks: ResolvedChunks {
                    default_task_count: 2,
                    target_runtime_seconds: None,
                    range_constraint: RangeConstraint::Noncontiguous,
                },
            },
        );
        if let Some(values) = other {
            defs.insert(
                "S".to_string(),
                TaskParameter::String {
                    range: values.into_iter().map(String::from).collect(),
                },
            );
        }
        StepParameterSpace {
            task_parameter_definitions: defs,
            combination: None,
        }
    }

    /// Run every chunk of `space` and map each of its values, as the CLI
    /// does, returning the per-Task index each (S, F) got, in chunk order.
    fn assign(space: &StepParameterSpace) -> Vec<(String, i64, usize)> {
        let mut index = ChunkTaskIndex::new(space).unwrap();
        let mut got = Vec::new();
        for action in StepParameterSpaceIterator::new(space).unwrap() {
            let ExprValue::RangeExpr(chunk) = &action["F"].value else {
                panic!("chunk is a range");
            };
            let s = action
                .get("S")
                .map(|v| v.value.to_display_string())
                .unwrap_or_default();
            for v in chunk.iter() {
                got.push((s.clone(), v, index.take(&action, Some(v)).unwrap()));
            }
        }
        got
    }

    /// The per-Task (S, F) of each index.
    fn per_task(space: &StepParameterSpace) -> Vec<(String, i64)> {
        StepParameterSpaceIterator::new_with_chunk_override(space, Some(1))
            .unwrap()
            .map(|set| {
                let s = set
                    .get("S")
                    .map(|v| v.value.to_display_string())
                    .unwrap_or_default();
                (s, single_int(&set["F"].value).unwrap())
            })
            .collect()
    }

    fn assert_aligned(space: &StepParameterSpace) {
        let tasks = per_task(space);
        let got = assign(space);
        assert_eq!(got.len(), tasks.len());
        let mut seen: Vec<usize> = got.iter().map(|(_, _, i)| *i).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..tasks.len()).collect::<Vec<_>>());
        for (s, v, i) in got {
            assert_eq!(tasks[i], (s, v), "index {i}");
        }
    }

    #[test]
    fn list_range_chunks_map_to_written_order() {
        // Chunks are {1, 5} and {2, 3}; the per-Task order is 5, 1, 3, 2.
        let space = space(vec![5, 1, 3, 2], None);
        assert_eq!(
            per_task(&space).iter().map(|t| t.1).collect::<Vec<_>>(),
            [5, 1, 3, 2]
        );
        let got = assign(&space);
        let by_value: HashMap<i64, usize> = got.iter().map(|(_, v, i)| (*v, *i)).collect();
        assert_eq!(by_value, HashMap::from([(5, 0), (1, 1), (3, 2), (2, 3)]));
        assert_aligned(&space);
    }

    #[test]
    fn chunk_crossed_with_a_second_parameter() {
        assert_aligned(&space(vec![5, 1, 3, 2], Some(vec!["a", "b"])));
    }

    #[test]
    fn repeated_second_parameter_values_get_distinct_indices() {
        assert_aligned(&space(vec![5, 1, 3, 2], Some(vec!["a", "a"])));
    }

    #[test]
    fn take_refuses_unknown_used_up_and_mismatched_values() {
        let space = space(vec![1, 2], None);
        let mut index = ChunkTaskIndex::new(&space).unwrap();
        assert_eq!(index.task_count(), 2);
        assert_eq!(index.chunk_parameter_name(), Some("F"));
        let action = StepParameterSpaceIterator::new(&space)
            .unwrap()
            .next()
            .unwrap();
        assert_eq!(index.take(&action, Some(9)), None);
        assert_eq!(index.take(&action, None), None);
        assert_eq!(index.take(&action, Some(1)), Some(0));
        assert_eq!(index.take(&action, Some(1)), None);
    }

    #[test]
    fn in_order_takes_walk_only_as_far_as_needed() {
        let space = space((0..1000).collect(), None);
        let mut index = ChunkTaskIndex::new(&space).unwrap();
        assert_eq!(index.task_count(), 1000);
        assert_eq!(index.walked, 0);
        let action = StepParameterSpaceIterator::new(&space)
            .unwrap()
            .next()
            .unwrap();
        assert_eq!(index.take(&action, Some(0)), Some(0));
        assert_eq!(index.take(&action, Some(1)), Some(1));
        assert_eq!(index.walked, 2);
        // A value not in the space walks to the end and finds nothing.
        assert_eq!(index.take(&action, Some(5000)), None);
        assert_eq!(index.walked, 1000);
        assert_eq!(index.take(&action, Some(7)), Some(7));
    }

    #[test]
    fn float_values_match_by_number_not_spelling() {
        let mut defs = IndexMap::new();
        defs.insert(
            "X".to_string(),
            TaskParameter::Float {
                range: vec![
                    openjd_expr::value::Float64::with_str(1.5, "1.50".into()).unwrap(),
                    openjd_expr::value::Float64::with_str(2.0, "2".into()).unwrap(),
                ],
            },
        );
        let space = StepParameterSpace {
            task_parameter_definitions: defs,
            combination: None,
        };
        let mut index = ChunkTaskIndex::new(&space).unwrap();
        let given = |x: f64| {
            let mut set = TaskParameterSet::new();
            set.insert(
                "X".to_string(),
                crate::types::TaskParameterValue {
                    param_type: crate::types::TaskParameterType::Float,
                    value: ExprValue::Float(openjd_expr::value::Float64::new(x).unwrap()),
                },
            );
            set
        };
        assert_eq!(index.take(&given(2.0), None), Some(1));
        assert_eq!(index.take(&given(1.5), None), Some(0));
    }

    #[test]
    fn unchunked_space_keys_by_every_parameter() {
        let mut defs = IndexMap::new();
        defs.insert(
            "S".to_string(),
            TaskParameter::String {
                range: vec!["x".into(), "y".into(), "x".into()],
            },
        );
        let space = StepParameterSpace {
            task_parameter_definitions: defs,
            combination: None,
        };
        let mut index = ChunkTaskIndex::new(&space).unwrap();
        assert_eq!(index.chunk_parameter_name(), None);
        let sets: Vec<_> = StepParameterSpaceIterator::new(&space).unwrap().collect();
        assert_eq!(index.take(&sets[0], Some(0)), None);
        assert_eq!(index.take(&sets[0], None), Some(0));
        assert_eq!(index.take(&sets[1], None), Some(1));
        assert_eq!(index.take(&sets[2], None), Some(2));
        assert_eq!(index.take(&sets[2], None), None);
    }
}
