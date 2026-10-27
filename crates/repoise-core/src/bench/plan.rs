//! Deterministic trial scheduling.
//!
//! Trials run sequentially. Within each (task, repetition) block the
//! condition order is a seeded Fisher–Yates permutation (splitmix64 stream),
//! so a recorded seed always replays the exact same schedule — no condition
//! runs all of its repetitions before another starts.

use super::suite::{Condition, Suite, Task};

/// One flattened, fully-ordered trial slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trial {
    /// Stable unique id: `t-<task>-r<rep>-<condition>` (sanitized).
    pub trial_id: String,
    pub task_index: usize,
    pub task_id: String,
    pub condition_index: usize,
    pub condition_id: String,
    /// 0-based repetition index.
    pub repetition: u32,
    /// Position in the global sequential schedule.
    pub ordered_index: usize,
}

/// The frozen schedule: one block per (task, repetition) pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrialPlan {
    pub seed: u64,
    pub blocks: Vec<Block>,
}

/// One (task, repetition) block with its seeded condition order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub task_index: usize,
    pub task_id: String,
    pub repetition: u32,
    /// Indices into `Suite::conditions` in execution order.
    pub order: Vec<usize>,
}

/// splitmix64: a tiny deterministic 64-bit stream (recorded in the plan so
/// seed replay is verifiable without re-implementing the generator).
pub fn splitmix64(mut state: u64) -> u64 {
    state = state.wrapping_add(0x9E3779B97F4A7C15);
    state = (state ^ (state >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94D049BB133111EB);
    state ^ (state >> 31)
}

/// Mixes the experiment seed with the block coordinates.
fn block_seed(seed: u64, task_index: usize, repetition: u32) -> u64 {
    let mut state = splitmix64(seed);
    state = splitmix64(state.wrapping_add(u64::from(task_index as u32).wrapping_add(1)));
    splitmix64(state.wrapping_add(u64::from(repetition).wrapping_add(1)))
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

impl TrialPlan {
    /// Builds the frozen schedule for one suite.
    pub fn new(suite: &Suite) -> Self {
        let mut blocks: Vec<Block> = Vec::new();
        for (task_index, task) in suite.tasks.iter().enumerate() {
            for repetition in 0..suite.repeat {
                let mut order: Vec<usize> = (0..suite.conditions.len()).collect();
                let mut state = block_seed(suite.seed, task_index, repetition);
                for i in (1..order.len()).rev() {
                    state = splitmix64(state);
                    let j = state % (u64::from(i as u32) + 1);
                    order.swap(i, j as usize);
                }
                blocks.push(Block {
                    task_index,
                    task_id: task.id.clone(),
                    repetition,
                    order,
                });
            }
        }
        Self {
            seed: suite.seed,
            blocks,
        }
    }

    /// The flat sequential trial list (block order, then in-block order).
    pub fn trials(&self, suite: &Suite) -> Vec<Trial> {
        let mut trials: Vec<Trial> = Vec::new();
        for block in &self.blocks {
            for &condition_index in &block.order {
                let task: &Task = &suite.tasks[block.task_index];
                let condition: &Condition = &suite.conditions[condition_index];
                trials.push(Trial {
                    trial_id: format!(
                        "t-{}-r{}-{}",
                        sanitize(&task.id),
                        block.repetition,
                        sanitize(&condition.id)
                    ),
                    task_index: block.task_index,
                    task_id: task.id.clone(),
                    condition_index,
                    condition_id: condition.id.clone(),
                    repetition: block.repetition,
                    ordered_index: trials.len(),
                });
            }
        }
        trials
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suite_with_seed(seed: u64) -> Suite {
        let value = serde_json::json!({
            "id": "exp",
            "root": "/tmp/repo",
            "tasks": [{ "id": "t", "prompt": "x" }],
            "conditions": [
                { "id": "a", "retrievalMode": "baseline" },
                { "id": "b", "retrievalMode": "baseline" },
                { "id": "c", "retrievalMode": "baseline" }
            ],
            "repeat": 2,
            "seed": seed,
            "adapter": { "executable": "node" },
            "profile": { "harness": { "name": "fake" } }
        });
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn seeded_shuffle_advances_the_stream() {
        // Hand-computed from the splitmix64 stream with one draw per swap;
        // the correlated-draws bug (reusing one draw for every swap) yields
        // [2, 1, 0] for BOTH seeds, so these orders pin the real stream.
        let plan = TrialPlan::new(&suite_with_seed(7));
        assert_eq!(plan.blocks[0].order, vec![2, 1, 0]);
        let plan = TrialPlan::new(&suite_with_seed(42));
        assert_eq!(plan.blocks[0].order, vec![1, 2, 0]);
        // A later repetition re-seeds its block.
        assert_eq!(plan.blocks[1].order, vec![2, 1, 0]);
    }

    #[test]
    fn same_seed_replays_the_same_plan() {
        let a = TrialPlan::new(&suite_with_seed(1234));
        let b = TrialPlan::new(&suite_with_seed(1234));
        assert_eq!(a, b);
    }
}
