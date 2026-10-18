// Состояние возврата: неизвестные старые условия не становятся доказательством нагрузки.
use machine::auto::{Protection, Trial, Workload};
use machine::thermal::Reading;
use machine::topology::Block;
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy)]
pub(super) struct LegacyCut {
    pub at: u64,
    pub accepted: i32,
    pub to: i32,
}

pub(super) fn protection_value(protection: Option<Protection>) -> Value {
    protection.map_or(Value::Null, |p| {
        json!({
            "held": p.held,
            "reference": p.reference.map(|w| json!({"busy": w.busy, "ratio": w.ratio,
                "global_busy": w.global_busy, "relative": w.relative})),
            "trial": p.trial.map(|t| json!({"from": t.from, "to": t.to})),
        })
    })
}

pub(super) fn protection_read(value: &Value) -> Option<Protection> {
    let held = integer(value.get("held")).filter(|v| *v > 0)?;
    let reference = value.get("reference").and_then(|w| {
        let busy = fraction(w.get("busy"))?;
        let ratio = w.get("ratio")?.as_f64().filter(|v| v.is_finite() && *v > 0.0)?;
        let global_busy = fraction(w.get("global_busy"))?;
        let relative = fraction(w.get("relative"))?;
        Some(Workload {
            busy,
            ratio,
            global_busy,
            relative,
        })
    });
    let trial = value.get("trial").and_then(|t| {
        Some(Trial {
            from: integer(t.get("from"))?,
            to: integer(t.get("to"))?,
        })
    });
    let trial = trial.filter(|t| t.from == held && held.checked_add(1) == Some(t.to));
    Some(Protection { held, reference, trial })
}

pub(super) fn list_value(list: Option<&[i64]>, index: usize) -> Option<i32> {
    list.and_then(|v| v.get(index)).and_then(|v| i32::try_from(*v).ok())
}

pub(super) fn integer(value: Option<&Value>) -> Option<i32> {
    value?.as_i64().and_then(|v| i32::try_from(v).ok())
}

fn fraction(value: Option<&Value>) -> Option<f64> {
    value?.as_f64().filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
}

pub(super) fn relative_activity(blocks: &[Block], readings: &[Option<Reading>], out: &mut Vec<Option<f64>>) {
    out.clear();
    for (b, block) in blocks.iter().enumerate() {
        let sum: Option<f64> = blocks
            .iter()
            .enumerate()
            .filter(|(_, peer)| peer.kind == block.kind)
            .map(|(i, _)| {
                readings
                    .get(i)
                    .and_then(|r| r.and_then(|r| r.busy))
                    .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
            })
            .try_fold(0.0, |sum, value| value.map(|v| sum + v));
        out.push(
            sum.filter(|v| *v > 0.0)
                .and_then(|sum| readings.get(b).and_then(|r| r.and_then(|r| r.busy)).map(|v| v / sum)),
        );
    }
}

/// Старый журнал — лишь причина удержать снижение, но не источник отсутствующей нагрузки.
pub(super) fn legacy_cuts(text: &str, blocks: &[Block]) -> Vec<Option<LegacyCut>> {
    let mut cuts = vec![None; blocks.len()];
    for line in text.lines() {
        let Ok(map) = serde_json::from_str::<Map<String, Value>>(line) else {
            continue;
        };
        match map.get("act").and_then(Value::as_str) {
            Some("emergency" | "target" | "lower") => {
                let Some(b) = map
                    .get("first")
                    .and_then(Value::as_u64)
                    .and_then(|i| blocks.iter().position(|b| u64::try_from(b.first).ok() == Some(i)))
                else {
                    continue;
                };
                if let (Some(at), Some(accepted), Some(to)) = (
                    map.get("at").and_then(Value::as_u64),
                    integer(map.get("accepted")),
                    integer(map.get("to")),
                ) {
                    cuts[b] = Some(LegacyCut { at, accepted, to });
                }
            }
            // Явное выключение — право владельца сбросить ограничения.
            Some("back") if map.get("why").and_then(Value::as_str) == Some("автомодуль выключен") => {
                cuts.fill(None);
            }
            _ => {}
        }
    }
    cuts
}

/// Менять историческое решение можно лишь при полной читаемой истории последующих правок.
pub(super) fn no_owner_change(text: &str, first: usize, cut: LegacyCut) -> bool {
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(map) = serde_json::from_str::<Map<String, Value>>(line) else {
            return false;
        };
        let Some(at) = map.get("at").and_then(Value::as_u64) else {
            return false;
        };
        if at <= cut.at {
            continue;
        }
        let act = map.get("act").and_then(Value::as_str);
        if matches!(act, Some("save" | "delete")) {
            continue;
        }
        let before = map
            .get("before")
            .and_then(Value::as_array)
            .and_then(|v| integer(v.get(first)));
        let after = map
            .get("after")
            .and_then(Value::as_array)
            .and_then(|v| integer(v.get(first)));
        if before.is_none() || after.is_none() || before != after {
            return false;
        }
        // Повторное применение полного профиля — тоже явный выбор, даже с теми же числами.
        if matches!(act, Some("menu" | "open" | "logon" | "accept" | "revert")) {
            return false;
        }
        if act == Some("limit")
            && map
                .get("block")
                .and_then(Value::as_str)
                .is_some_and(|name| name.split('–').next() == Some(&first.to_string()))
        {
            return false;
        }
    }
    true
}

// Независимые проверки сохранения и относительной активности по контракту API.
// Независимые проверки сохранения и относительной активности по контракту API.
#[cfg(test)]
mod independent_return_specs {
    use super::{Block, LegacyCut, legacy_cuts, no_owner_change, protection_read, protection_value, relative_activity};
    use machine::auto::{Protection, Trial, Workload};
    use machine::thermal::Reading;
    use machine::topology::Kind;
    use serde_json::json;

    fn block(first: usize, kind: Kind) -> Block {
        Block {
            members: vec![first],
            first,
            lp: machine::Lp(u32::try_from(first).expect("небольшой индекс стенда")),
            kind,
            name: format!("core{first}"),
        }
    }

    fn reading(busy: Option<f64>) -> Reading {
        Reading {
            temp: Some(60.0),
            ratio: Some(49.0),
            busy,
        }
    }

    #[test]
    fn protection_preserves_exact_workload_not_display_rounding() {
        let saved = Protection {
            held: 49,
            reference: Some(Workload {
                busy: 0.887_654_321_098_76,
                ratio: 50.512_345_678_901_2,
                global_busy: 0.612_345_678_912_345,
                relative: 0.137_654_321_098_7,
            }),
            trial: Some(Trial { from: 49, to: 50 }),
        };
        let text = serde_json::to_string(&protection_value(Some(saved))).expect("запись JSON");
        let decoded = serde_json::from_str(&text).expect("чтение JSON");
        assert_eq!(protection_read(&decoded), Some(saved));
    }

    #[test]
    fn legacy_or_missing_workload_retains_held_cap_as_unknown() {
        for value in [json!({"held":49}), json!({"held":49,"reference":null,"trial":null})] {
            let saved = protection_read(&value).expect("сохранённое ограничение должно читаться");
            assert_eq!(saved.held, 49);
            assert!(saved.reference.is_none());
            assert!(saved.trial.is_none());
        }
        assert!(protection_read(&serde_json::Value::Null).is_none());
        assert_eq!(protection_value(None), serde_json::Value::Null);
    }

    #[test]
    fn malformed_workload_is_unknown_without_losing_protective_cap() {
        for reference in [
            json!({"busy":0.9,"ratio":49.0,"global_busy":null,"relative":0.5}),
            json!({"busy":1.1,"ratio":49.0,"global_busy":0.6,"relative":0.5}),
            json!({"busy":0.9,"ratio":-2.0,"global_busy":0.6,"relative":0.5}),
            json!({"busy":0.9,"ratio":49.0,"global_busy":0.6,"relative":2.0}),
        ] {
            let saved =
                protection_read(&json!({"held":49,"reference":reference})).expect("ограничение не должно исчезнуть");
            assert_eq!(saved.held, 49);
            assert!(saved.reference.is_none());
        }
        assert!(protection_read(&json!({"held":0})).is_none());
    }

    #[test]
    fn relative_activity_keeps_p_and_e_reference_groups_separate() {
        let blocks = [
            block(0, Kind::P),
            block(1, Kind::P),
            block(2, Kind::E),
            block(3, Kind::E),
        ];
        let readings = [
            Some(reading(Some(1.0))),
            Some(reading(Some(0.5))),
            Some(reading(Some(0.25))),
            Some(reading(Some(0.75))),
        ];
        let mut out = Vec::new();
        relative_activity(&blocks, &readings, &mut out);
        assert_eq!(out.len(), 4);
        for (actual, expected) in out.iter().zip([2.0 / 3.0, 1.0 / 3.0, 0.25, 0.75]) {
            assert!(actual.is_some_and(|value| (value - expected).abs() < 1e-12));
        }
    }

    #[test]
    fn missing_peer_is_not_zero_activity_but_other_kind_remains_known() {
        let blocks = [block(0, Kind::P), block(1, Kind::P), block(2, Kind::E)];
        let readings = [Some(reading(Some(0.8))), Some(reading(None)), Some(reading(Some(0.5)))];
        let mut out = vec![Some(0.0); 8];
        relative_activity(&blocks, &readings, &mut out);
        assert_eq!(out, [None, None, Some(1.0)]);
    }

    #[test]
    fn all_idle_relative_profile_is_undefined() {
        let blocks = [block(0, Kind::P), block(1, Kind::P)];
        let mut out = Vec::new();
        relative_activity(&blocks, &[Some(reading(Some(0.0))), Some(reading(Some(0.0)))], &mut out);
        assert_eq!(out, [None, None]);
    }

    #[test]
    fn invalid_peer_activity_cannot_create_plausible_relative_shares() {
        let blocks = [block(0, Kind::P), block(1, Kind::P)];
        for invalid in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
            let mut out = Vec::new();
            relative_activity(
                &blocks,
                &[Some(reading(Some(0.8))), Some(reading(Some(invalid)))],
                &mut out,
            );
            assert_eq!(out, [None, None]);
        }
    }

    #[test]
    fn latest_legacy_cut_is_retained_despite_old_automatic_returns() {
        let blocks = [block(0, Kind::P)];
        let text = [
            json!({"act":"emergency","at":100,"first":0,"accepted":54,"to":49}).to_string(),
            json!({"act":"return","at":200,"first":0,"accepted":54,"to":50}).to_string(),
            json!({"act":"target","at":300,"first":0,"accepted":54,"to":48}).to_string(),
            json!({"act":"return","at":400,"first":0,"accepted":54,"to":54}).to_string(),
        ]
        .join("\n");
        let cuts = legacy_cuts(&text, &blocks);
        let cut = cuts[0].expect("прежний возврат не отменяет факт перегрева");
        assert_eq!((cut.at, cut.accepted, cut.to), (300, 54, 48));
    }

    #[test]
    fn explicit_auto_off_clears_legacy_migration_before_new_cuts() {
        let blocks = [block(0, Kind::P)];
        let text = [
            json!({"act":"emergency","at":100,"first":0,"accepted":54,"to":49}).to_string(),
            json!({"act":"back","at":200,"why":"автомодуль выключен"}).to_string(),
        ]
        .join("\n");
        assert!(legacy_cuts(&text, &blocks)[0].is_none());
        let text = format!(
            "{text}\n{}",
            json!({"act":"target","at":300,"first":0,"accepted":53,"to":47})
        );
        let cut = legacy_cuts(&text, &blocks)[0].expect("новая отсечка после явного сброса");
        assert_eq!((cut.at, cut.accepted, cut.to), (300, 53, 47));
    }

    #[test]
    fn full_profile_application_overrides_even_identical_values() {
        let cut = LegacyCut {
            at: 100,
            accepted: 54,
            to: 49,
        };
        let text = json!({"act":"open","at":200,"before":[54,53],"after":[54,53]}).to_string();
        assert!(!no_owner_change(&text, 0, cut));
        let old = json!({"act":"open","at":50,"before":[54,53],"after":[54,53]}).to_string();
        assert!(no_owner_change(&old, 0, cut));
    }

    #[test]
    fn corrupt_owner_history_cannot_authorize_legacy_migration() {
        let cut = LegacyCut {
            at: 100,
            accepted: 54,
            to: 49,
        };
        for text in ["{broken", "{\"act\":\"open\",\"before\":[54],\"after\":[54]}"] {
            assert!(!no_owner_change(text, 0, cut));
        }
    }
}
