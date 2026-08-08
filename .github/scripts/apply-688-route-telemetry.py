from pathlib import Path


def replace(path, old, new):
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f'missing fragment in {path}: {old[:80]!r}')
    p.write_text(text.replace(old, new, 1))

replace('crates/medusa-provider/src/route_latency.rs',
'''    pub cached_input_tokens: u64,
    pub input_tokens: u64,
}''',
'''    pub cached_input_tokens: u64,
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub generation_total_ms: u64,
    #[serde(default)]
    pub retry_attempts: u64,
    #[serde(default)]
    pub retry_recoveries: u64,
}''')

replace('crates/medusa-provider/src/route_latency.rs',
'''    pub fn success_milli(self) -> u16 {
        let attempts = self.successes.saturating_add(self.failures);
        if attempts == 0 {
            return 1_000;
        }
        ((u128::from(self.successes) * 1_000 / u128::from(attempts)) as u16).min(1_000)
    }
}''',
'''    pub fn success_milli(self) -> u16 {
        let attempts = self.successes.saturating_add(self.failures);
        if attempts == 0 {
            return 1_000;
        }
        ((u128::from(self.successes) * 1_000 / u128::from(attempts)) as u16).min(1_000)
    }

    /// Observed output throughput in milli-tokens per second.
    #[must_use]
    pub fn output_tokens_per_second_milli(self) -> Option<u64> {
        (self.generation_total_ms > 0 && self.output_tokens > 0).then(|| {
            let scaled = u128::from(self.output_tokens)
                .saturating_mul(1_000_000)
                / u128::from(self.generation_total_ms);
            scaled.min(u128::from(u64::MAX)) as u64
        })
    }

    /// Share of retry attempts that eventually recovered on the same route.
    #[must_use]
    pub fn retry_recovery_milli(self) -> u16 {
        if self.retry_attempts == 0 {
            return 1_000;
        }
        ((u128::from(self.retry_recoveries.min(self.retry_attempts)) * 1_000
            / u128::from(self.retry_attempts)) as u16)
            .min(1_000)
    }
}''')

replace('crates/medusa-provider/src/route_latency.rs',
'''        .map(|(index, _)| {
            let stats = stats.get(index).copied().unwrap_or_default();
            (index, expected_latency_ms(stats, policy))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(index, score)| (*score, *index));
    candidates.into_iter().map(|(index, _)| index).collect()''',
'''        .map(|(index, _)| {
            let stats = stats.get(index).copied().unwrap_or_default();
            (
                index,
                expected_latency_ms(stats, policy),
                stats.output_tokens_per_second_milli().unwrap_or_default(),
                stats.retry_recovery_milli(),
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(index, score, throughput, recovery)| {
        (*score, std::cmp::Reverse(*throughput), std::cmp::Reverse(*recovery), *index)
    });
    candidates
        .into_iter()
        .map(|(index, _, _, _)| index)
        .collect()''')

replace('crates/medusa-provider/src/route_latency.rs',
'''    fn cold_route_ties_preserve_configured_order() {
        let profiles = vec![profile("a", true, false), profile("b", true, false)];
        assert_eq!(
            latency_aware_route_order(&profiles, &[], true, false, RouteLatencyPolicy::default()),
            vec![0, 1]
        );
    }
}''',
'''    fn cold_route_ties_preserve_configured_order() {
        let profiles = vec![profile("a", true, false), profile("b", true, false)];
        assert_eq!(
            latency_aware_route_order(&profiles, &[], true, false, RouteLatencyPolicy::default()),
            vec![0, 1]
        );
    }

    #[test]
    fn equal_latency_prefers_throughput_then_retry_recovery() {
        let profiles = vec![
            profile("slow-output", true, true),
            profile("fast-output", true, true),
            profile("recovered", true, true),
        ];
        let base = RouteLatencyStats {
            samples: 10,
            successes: 10,
            total_duration_ms: 10_000,
            output_tokens: 1_000,
            generation_total_ms: 10_000,
            retry_attempts: 10,
            retry_recoveries: 5,
            ..RouteLatencyStats::default()
        };
        let stats = vec![
            base,
            RouteLatencyStats {
                output_tokens: 2_000,
                ..base
            },
            RouteLatencyStats {
                retry_recoveries: 10,
                ..base
            },
        ];
        assert_eq!(
            latency_aware_route_order(&profiles, &stats, true, true, RouteLatencyPolicy::default()),
            vec![1, 2, 0]
        );
        assert_eq!(stats[1].output_tokens_per_second_milli(), Some(200_000));
        assert_eq!(stats[2].retry_recovery_milli(), 1_000);
    }
}''')

replace('crates/medusa-provider/src/route_metrics_store.rs',
'''            stats.input_tokens = stats.input_tokens.saturating_add(usage.input_tokens);
            stats.cached_input_tokens = stats
                .cached_input_tokens
                .saturating_add(usage.cache_read_input_tokens);
        })
    }

    pub fn record_failure''',
'''            stats.input_tokens = stats.input_tokens.saturating_add(usage.input_tokens);
            stats.cached_input_tokens = stats
                .cached_input_tokens
                .saturating_add(usage.cache_read_input_tokens);
            stats.output_tokens = stats.output_tokens.saturating_add(usage.output_tokens);
            let generation_ms = duration_ms.saturating_sub(first_token_ms.unwrap_or_default());
            stats.generation_total_ms = stats.generation_total_ms.saturating_add(generation_ms);
        })
    }

    pub fn record_retry_attempt(&self, index: usize) -> MedusaResult<()> {
        self.update(index, |stats| {
            stats.retry_attempts = stats.retry_attempts.saturating_add(1);
        })
    }

    pub fn record_retry_recovery(&self, index: usize) -> MedusaResult<()> {
        self.update(index, |stats| {
            stats.retry_recoveries = stats.retry_recoveries.saturating_add(1);
        })
    }

    pub fn record_failure''')

replace('crates/medusa-provider/src/route_metrics_store.rs',
'''        assert_eq!(stats.total_duration_ms, 120);
        assert_eq!(stats.cache_reuse_milli(), 900);''',
'''        assert_eq!(stats.total_duration_ms, 120);
        assert_eq!(stats.cache_reuse_milli(), 900);
        assert_eq!(stats.output_tokens, 0);
        assert_eq!(stats.generation_total_ms, 120);''')

replace('crates/medusa-provider/src/manager.rs',
'''                    Ok(response) => {
                        let duration_ms = elapsed_ms(started);
                        self.latency.record_success_with_first_token(''',
'''                    Ok(response) => {
                        let duration_ms = elapsed_ms(started);
                        if attempt > 0 {
                            self.latency.record_retry_recovery(index)?;
                        }
                        self.latency.record_success_with_first_token(''')

replace('crates/medusa-provider/src/manager.rs',
'''                                self.record_retry(index, delay_ms)?;
                                if let Some(flag) = cancel {''',
'''                                self.record_retry(index, delay_ms)?;
                                self.latency.record_retry_attempt(index)?;
                                if let Some(flag) = cancel {''')

replace('crates/medusa-provider/src/manager.rs',
'''        assert_eq!(manager.route_latency()[0].failures, 2);
        assert_eq!(manager.route_latency()[1].successes, 1);''',
'''        assert_eq!(manager.route_latency()[0].failures, 2);
        assert_eq!(manager.route_latency()[0].retry_attempts, 1);
        assert_eq!(manager.route_latency()[0].retry_recoveries, 0);
        assert_eq!(manager.route_latency()[1].successes, 1);''')
