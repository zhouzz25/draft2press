use crate::config::Pricing;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenUsage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    #[allow(dead_code)]
    pub total_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct CostTracker {
    pricing: Pricing,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_cost: f64,
    pub call_count: usize,
}

impl CostTracker {
    pub fn new(pricing: Pricing) -> Self {
        Self {
            pricing,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cost: 0.0,
            call_count: 0,
        }
    }

    pub fn record(&mut self, usage: &TokenUsage) -> f64 {
        let cost =
            (usage.prompt_tokens as f64 / 1000.0) * self.pricing.input_per_1k
                + (usage.completion_tokens as f64 / 1000.0) * self.pricing.output_per_1k;

        self.total_input_tokens += usage.prompt_tokens;
        self.total_output_tokens += usage.completion_tokens;
        self.total_cost += cost;
        self.call_count += 1;
        cost
    }

    pub fn total_tokens(&self) -> usize {
        self.total_input_tokens + self.total_output_tokens
    }

    #[allow(dead_code)]
    pub fn summary(&self) -> String {
        format!(
            "API 调用 {} 次 | 输入: {} tok | 输出: {} tok | 总计: {} tok | 费用: ${:.4}",
            self.call_count,
            self.total_input_tokens,
            self.total_output_tokens,
            self.total_tokens(),
            self.total_cost
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pricing() -> Pricing {
        Pricing {
            input_per_1k: 0.15,
            output_per_1k: 0.60,
        }
    }

    #[test]
    fn record_single_call() {
        let mut tracker = CostTracker::new(test_pricing());
        let usage = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        };
        let cost = tracker.record(&usage);
        assert!((cost - (0.15 + 0.30)).abs() < 0.0001);
        assert_eq!(tracker.total_tokens(), 1500);
        assert_eq!(tracker.call_count, 1);
    }

    #[test]
    fn record_multiple_calls() {
        let mut tracker = CostTracker::new(test_pricing());
        tracker.record(&TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 500,
            total_tokens: 1500,
        });
        tracker.record(&TokenUsage {
            prompt_tokens: 2000,
            completion_tokens: 1000,
            total_tokens: 3000,
        });
        assert_eq!(tracker.total_input_tokens, 3000);
        assert_eq!(tracker.total_output_tokens, 1500);
        assert_eq!(tracker.total_tokens(), 4500);
        assert_eq!(tracker.call_count, 2);
    }
}
