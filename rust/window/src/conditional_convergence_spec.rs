use crate::conditional::{Error, Metadata, Surface, fit, fit_iterations};
use crate::conditional_data::Prepared;
use serde_json::{Value, json};

struct Fixture {
    data: Prepared,
    template: Surface,
    train: Vec<bool>,
    ridge: f64,
}

fn fixture() -> Fixture {
    let mut value: Value = serde_json::from_str(include_str!("conditional-convergence-fixture.json"))
        .expect("синтетические исходные данные");
    let metadata: Metadata = serde_json::from_value(value["metadata"].take()).expect("метаданные");
    let bands = serde_json::from_value(value["bands"].take()).expect("полосы истории");
    let train = serde_json::from_value(value["train"].take()).expect("обучающая маска");
    let ridge = value["ridge"].as_f64().expect("штраф");
    let mut input = value["data"].take();
    if let Some(package) = input.as_object_mut().expect("таблица").remove("package_lag_only") {
        input["contexts"]["package_lag_only"] = package;
    }
    Fixture {
        data: serde_json::from_value(input).expect("подготовленные данные"),
        template: Surface::template(metadata, bands),
        train,
        ridge,
    }
}

fn status(model: &Surface) -> Value {
    let value = serde_json::to_value(model).expect("модель со статусом");
    assert!(
        value["fit_status"].is_object(),
        "результат подгонки должен иметь явный статус"
    );
    value["fit_status"].clone()
}

fn excludes_diagnostic(diagnostics: &Value, poison: &Value) -> bool {
    !diagnostics.as_array().expect("массив диагностики").contains(poison)
}

#[test]
fn outer_cap_zero_is_provisional_and_clears_previous_diagnostics_and_status() {
    let case = fixture();
    let mut old = serde_json::to_value(case.template).expect("шаблон");
    let poison = json!({"iterations": 9999, "projected_gradient": 1234.0});
    old["diagnostics"] = json!([poison]);
    old["fit_status"] = json!({
        "converged": true,
        "outer_iterations": 999,
        "max_kernel_change": 0.0,
        "stop": "kernel_stable"
    });
    let poisoned: Surface = serde_json::from_value(old).expect("старый статус не означает новый результат");
    let old_diagnostics = serde_json::to_value(&poisoned.diagnostics).expect("старая диагностика");
    assert!(
        !excludes_diagnostic(&old_diagnostics, &poison),
        "отрицательный контроль: сохранённая старая запись обязана отвергаться"
    );
    let provisional = fit_iterations(&case.data, poisoned, &case.train, case.ridge, 0)
        .expect("нулевой явный лимит допускает предварительную модель");
    let state = status(&provisional);
    assert_eq!(state["converged"], json!(false));
    assert_eq!(state["outer_iterations"], json!(0));
    assert_eq!(state["max_kernel_change"], Value::Null);
    assert_eq!(state["stop"], json!("iteration_limit"));
    let fresh_diagnostics = serde_json::to_value(&provisional.diagnostics).expect("текущая диагностика");
    assert!(
        excludes_diagnostic(&fresh_diagnostics, &poison),
        "сохранена диагностика старого результата"
    );
    assert!(
        provisional
            .diagnostics
            .iter()
            .all(|diagnostic| { diagnostic.projected_gradient.is_finite() && diagnostic.projected_gradient >= 0.0 })
    );
}

#[test]
fn outer_default_fit_reports_stability_on_the_independent_synthetic_fixture() {
    let case = fixture();
    let fitted = fit(&case.data, case.template, &case.train, case.ridge)
        .expect("эталон достигает устойчивого ядра в стандартном лимите");
    let state = status(&fitted);
    assert_eq!(state["converged"], json!(true));
    assert_eq!(state["stop"], json!("kernel_stable"));
    let iterations = state["outer_iterations"].as_u64().expect("число внешних итераций");
    assert!((1..=12).contains(&iterations));
    let change = state["max_kernel_change"].as_f64().expect("измеренное изменение ядра");
    assert!(change.is_finite() && change >= 0.0);
}

#[test]
fn outer_eight_iterations_do_not_claim_kernel_stability_just_because_inner_fits_converge() {
    let case = fixture();
    let provisional = fit_iterations(&case.data, case.template, &case.train, case.ridge, 8)
        .expect("явно ограниченный расчёт возвращает предварительный результат");
    let state = status(&provisional);
    assert_eq!(state["converged"], json!(false));
    assert_eq!(state["stop"], json!("iteration_limit"));
    assert_eq!(state["outer_iterations"], json!(8));
    let change = state["max_kernel_change"]
        .as_f64()
        .expect("сравнение двух ядер уже состоялось");
    assert!(change.is_finite() && change > 0.0);
    assert!(
        !provisional.diagnostics.is_empty(),
        "внутренние подгонки действительно были"
    );
}

#[test]
fn outer_no_own_drive_cannot_identify_a_kernel_and_default_fit_refuses_it() {
    let mut case = fixture();
    case.data.busy.fill(0.0);
    let provisional = fit_iterations(&case.data, case.template, &case.train, case.ridge, 1)
        .expect("предварительный результат явно описывает отсутствие собственного сигнала");
    let state = status(&provisional);
    assert_eq!(state["converged"], json!(false));
    assert_eq!(state["stop"], json!("own_signal_unresolved"));
    assert_eq!(state["outer_iterations"], json!(0));
    assert_eq!(state["max_kernel_change"], Value::Null);

    let mut case = fixture();
    case.data.busy.fill(0.0);
    assert!(matches!(
        fit(&case.data, case.template, &case.train, case.ridge),
        Err(Error::NotConverged)
    ));
}

#[test]
fn outer_old_evaluation_only_model_loads_without_a_fabricated_fit_status() {
    let case = fixture();
    assert!(case.template.fit_status.is_none());
    let mut old = serde_json::to_value(case.template).expect("шаблон модели");
    let removed = old.as_object_mut().expect("модель").remove("fit_status");
    drop(removed);
    let decoded: Surface = serde_json::from_value(old).expect("старый формат без статуса читается");
    assert!(
        decoded.fit_status.is_none(),
        "старый файл не доказывает сходимость нового расчёта"
    );
}
