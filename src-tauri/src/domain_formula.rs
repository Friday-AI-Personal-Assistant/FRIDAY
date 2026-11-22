//! Deterministic, read-only domain formula engine.
//!
//! Monetary values use a bounded fixed-point representation (six decimal places)
//! backed by checked i128 arithmetic. No floating point, expression parser, model,
//! network access, file access, or workbook mutation is involved in this module.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const DECIMAL_SCALE: u32 = 6;
const DECIMAL_FACTOR: i128 = 1_000_000;
const MAX_INPUTS: usize = 16;
const MAX_INPUT_NAME_CHARS: usize = 64;
const MAX_VALUE_CHARS: usize = 48;
const MAX_CURRENCY_CHARS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Decimal {
    micros: i128,
}

impl Decimal {
    fn parse(source: &str) -> Result<Self, String> {
        let value = source.trim();
        if value.is_empty() || value.len() > MAX_VALUE_CHARS {
            return Err("Enter a decimal value within the supported length.".into());
        }
        let (negative, unsigned) = match value.as_bytes()[0] {
            b'-' => (true, &value[1..]),
            b'+' => (false, &value[1..]),
            _ => (false, value),
        };
        if unsigned.is_empty() {
            return Err("The decimal value is incomplete.".into());
        }
        let mut parts = unsigned.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            || fraction.len() > DECIMAL_SCALE as usize
        {
            return Err("Use a plain decimal with no more than six fractional digits.".into());
        }
        let whole_value = whole.parse::<i128>()
            .map_err(|_| "The decimal value is outside the supported range.".to_string())?;
        let mut fractional_text = fraction.to_string();
        while fractional_text.len() < DECIMAL_SCALE as usize {
            fractional_text.push('0');
        }
        let fractional_value = if fractional_text.is_empty() { 0 } else {
            fractional_text.parse::<i128>()
                .map_err(|_| "The decimal value is invalid.".to_string())?
        };
        let magnitude = whole_value.checked_mul(DECIMAL_FACTOR)
            .and_then(|value| value.checked_add(fractional_value))
            .ok_or_else(|| "The decimal value is outside the supported range.".to_string())?;
        let micros = if negative {
            magnitude.checked_neg().ok_or_else(|| "The decimal value is outside the supported range.".to_string())?
        } else { magnitude };
        Ok(Self { micros })
    }

    fn checked_add(self, other: Self) -> Result<Self, String> {
        self.micros.checked_add(other.micros)
            .map(|micros| Self { micros })
            .ok_or_else(|| "The calculation exceeds the supported numeric range.".into())
    }

    fn checked_sub(self, other: Self) -> Result<Self, String> {
        self.micros.checked_sub(other.micros)
            .map(|micros| Self { micros })
            .ok_or_else(|| "The calculation exceeds the supported numeric range.".into())
    }

    fn checked_mul(self, other: Self) -> Result<Self, String> {
        let product = self.micros.checked_mul(other.micros)
            .ok_or_else(|| "The calculation exceeds the supported numeric range.".to_string())?;
        Ok(Self { micros: round_divide(product, DECIMAL_FACTOR)? })
    }

    fn checked_div(self, other: Self) -> Result<Self, String> {
        if other.micros == 0 {
            return Err("Division by zero is not allowed.".into());
        }
        let numerator = self.micros.checked_mul(DECIMAL_FACTOR)
            .ok_or_else(|| "The calculation exceeds the supported numeric range.".to_string())?;
        Ok(Self { micros: round_divide(numerator, other.micros)? })
    }

    fn format(self) -> String {
        let negative = self.micros < 0;
        let magnitude = self.micros.unsigned_abs();
        let whole = magnitude / DECIMAL_FACTOR as u128;
        let fraction = magnitude % DECIMAL_FACTOR as u128;
        if fraction == 0 {
            return format!("{}{whole}", if negative { "-" } else { "" });
        }
        let mut fractional = format!("{fraction:06}");
        while fractional.ends_with('0') { fractional.pop(); }
        format!("{}{whole}.{fractional}", if negative { "-" } else { "" })
    }
}

fn round_divide(numerator: i128, denominator: i128) -> Result<i128, String> {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let twice_remainder = remainder.unsigned_abs().checked_mul(2)
        .ok_or_else(|| "The calculation exceeds the supported numeric range.".to_string())?;
    if twice_remainder >= denominator.unsigned_abs() {
        let same_sign = (numerator < 0) == (denominator < 0);
        quotient.checked_add(if same_sign { 1 } else { -1 })
            .ok_or_else(|| "The calculation exceeds the supported numeric range.".into())
    } else {
        Ok(quotient)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Unit {
    Currency,
    Percent,
}

#[derive(Clone, Debug)]
struct FormulaInputDefinition {
    name: &'static str,
    unit: Unit,
}

#[derive(Clone, Debug)]
struct FormulaDefinition {
    id: &'static str,
    version: u16,
    domain: &'static str,
    name: &'static str,
    description: &'static str,
    inputs: &'static [FormulaInputDefinition],
    output_unit: Unit,
    rounding: &'static str,
}

const NET_SALES_INPUTS: &[FormulaInputDefinition] = &[
    FormulaInputDefinition { name: "gross_sales", unit: Unit::Currency },
    FormulaInputDefinition { name: "discounts", unit: Unit::Currency },
    FormulaInputDefinition { name: "sales_returns", unit: Unit::Currency },
];
const GROSS_PROFIT_INPUTS: &[FormulaInputDefinition] = &[
    FormulaInputDefinition { name: "net_sales", unit: Unit::Currency },
    FormulaInputDefinition { name: "cost_of_goods_sold", unit: Unit::Currency },
];
const GROSS_MARGIN_INPUTS: &[FormulaInputDefinition] = &[
    FormulaInputDefinition { name: "net_sales", unit: Unit::Currency },
    FormulaInputDefinition { name: "cost_of_goods_sold", unit: Unit::Currency },
];

const FORMULAS: &[FormulaDefinition] = &[
    FormulaDefinition {
        id: "accounting.net_sales", version: 1, domain: "accounting",
        name: "Net sales", description: "Gross sales less discounts and sales returns.",
        inputs: NET_SALES_INPUTS, output_unit: Unit::Currency,
        rounding: "Exact fixed-point arithmetic; preserve input precision up to six decimal places.",
    },
    FormulaDefinition {
        id: "accounting.gross_profit", version: 1, domain: "accounting",
        name: "Gross profit", description: "Net sales less cost of goods sold.",
        inputs: GROSS_PROFIT_INPUTS, output_unit: Unit::Currency,
        rounding: "Exact fixed-point arithmetic; preserve input precision up to six decimal places.",
    },
    FormulaDefinition {
        id: "accounting.gross_margin", version: 1, domain: "accounting",
        name: "Gross margin", description: "Gross profit divided by net sales, multiplied by 100.",
        inputs: GROSS_MARGIN_INPUTS, output_unit: Unit::Percent,
        rounding: "Six decimal places; midpoint ties round away from zero.",
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueState {
    Provided,
    Missing,
    Unknown,
    NotApplicable,
}

impl ValueState {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "provided" => Ok(Self::Provided),
            "missing" => Ok(Self::Missing),
            "unknown" => Ok(Self::Unknown),
            "not_applicable" => Ok(Self::NotApplicable),
            _ => Err("Input state must be provided, missing, unknown, or not_applicable.".into()),
        }
    }
}

#[derive(Clone, Debug)]
struct CalculationInput {
    name: String,
    state: ValueState,
    value: Option<String>,
    unit: Unit,
    currency: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FormulaSummary {
    pub id: &'static str,
    pub version: u16,
    pub domain: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub required_inputs: Vec<&'static str>,
    pub output_unit: &'static str,
    pub rounding_policy: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalculationResult {
    pub formula_id: String,
    pub formula_version: u16,
    pub status: String,
    pub output_value: Option<String>,
    pub output_unit: String,
    pub currency: Option<String>,
    pub missing_inputs: Vec<String>,
    pub trace: Vec<String>,
    pub warnings: Vec<String>,
}

fn unit_name(unit: &Unit) -> &'static str {
    match unit { Unit::Currency => "currency", Unit::Percent => "percent" }
}

fn formula(id: &str) -> Result<&'static FormulaDefinition, String> {
    FORMULAS.iter().find(|item| item.id == id)
        .ok_or_else(|| "This formula is not in FRIDAY's reviewed formula registry.".into())
}

fn validate_currency(currency: &str) -> Result<(), String> {
    if currency.len() != MAX_CURRENCY_CHARS || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err("Currency must be an explicit three-letter uppercase code, such as USD, GBP, or PKR.".into());
    }
    Ok(())
}

fn validate_input(input: &CalculationInput) -> Result<(), String> {
    if input.name.is_empty() || input.name.len() > MAX_INPUT_NAME_CHARS
        || !input.name.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
        return Err("Input names must use lowercase letters, digits, and underscores.".into());
    }
    match input.state {
        ValueState::Provided if input.value.is_none() =>
            return Err(format!("Input '{}' is marked provided but has no value.", input.name)),
        ValueState::Provided => {
            let raw = input.value.as_deref().unwrap_or_default();
            if raw.len() > MAX_VALUE_CHARS { return Err(format!("Input '{}' exceeds the value length limit.", input.name)); }
            Decimal::parse(raw)?;
        }
        _ if input.value.is_some() =>
            return Err(format!("Input '{}' has a value but is not marked provided.", input.name)),
        _ => {}
    }
    if input.unit == Unit::Currency {
        let currency = input.currency.as_deref()
            .ok_or_else(|| format!("Input '{}' requires an explicit currency.", input.name))?;
        validate_currency(currency)?;
    } else if input.currency.is_some() {
        return Err(format!("Input '{}' must not attach a currency to a non-currency unit.", input.name));
    }
    Ok(())
}

fn result_incomplete(definition: &FormulaDefinition, missing: Vec<String>, trace: Vec<String>) -> CalculationResult {
    CalculationResult {
        formula_id: definition.id.into(), formula_version: definition.version, status: "incomplete".into(),
        output_value: None, output_unit: unit_name(&definition.output_unit).into(), currency: None,
        missing_inputs: missing, trace, warnings: vec!["No calculation was performed because required inputs are missing, unknown, or not applicable.".into()],
    }
}

pub(crate) fn sum_currency_values(values: &[String]) -> Result<String, String> {
    if values.is_empty() {
        return Err("No numeric source values were available for this accounting input.".into());
    }
    let mut total = Decimal { micros: 0 };
    for value in values {
        total = total.checked_add(Decimal::parse(value)?)?;
    }
    Ok(total.format())
}

pub(crate) fn calculate_from_workbook(
    formula_id: &str,
    currency: &str,
    values: Vec<(String, String)>,
) -> Result<CalculationResult, String> {
    let inputs = values.into_iter().map(|(name, value)| CalculationInput {
        name,
        state: ValueState::Provided,
        value: Some(value),
        unit: Unit::Currency,
        currency: Some(currency.to_string()),
    }).collect();
    calculate(formula_id, inputs)
}

pub(crate) fn formula_registry() -> Vec<FormulaSummary> {
    FORMULAS.iter().map(|definition| FormulaSummary {
        id: definition.id, version: definition.version, domain: definition.domain,
        name: definition.name, description: definition.description,
        required_inputs: definition.inputs.iter().map(|input| input.name).collect(),
        output_unit: unit_name(&definition.output_unit), rounding_policy: definition.rounding,
    }).collect()
}

fn calculate(formula_id: &str, inputs: Vec<CalculationInput>) -> Result<CalculationResult, String> {
    let definition = formula(formula_id)?;
    if inputs.is_empty() || inputs.len() > MAX_INPUTS {
        return Err("Supply a bounded set of formula inputs.".into());
    }
    let mut by_name = BTreeMap::new();
    for input in inputs {
        validate_input(&input)?;
        if by_name.insert(input.name.clone(), input).is_some() {
            return Err("Duplicate formula input names are not allowed.".into());
        }
    }
    let required = definition.inputs.iter().map(|item| item.name).collect::<BTreeSet<_>>();
    if by_name.keys().any(|name| !required.contains(name.as_str())) {
        return Err("The request contains an input that this formula does not accept.".into());
    }
    let mut missing = Vec::new();
    let mut trace = vec![format!("Formula: {} v{}", definition.id, definition.version)];
    let mut currency: Option<String> = None;
    let mut values = BTreeMap::new();
    for expected in definition.inputs {
        let Some(input) = by_name.get(expected.name) else {
            missing.push(expected.name.to_string());
            continue;
        };
        if input.unit != expected.unit {
            return Err(format!("Input '{}' has an incompatible unit.", expected.name));
        }
        if input.state != ValueState::Provided {
            missing.push(format!("{} ({})", expected.name, match input.state {
                ValueState::Missing => "missing",
                ValueState::Unknown => "unknown",
                ValueState::NotApplicable => "not applicable",
                ValueState::Provided => "provided",
            }));
            continue;
        }
        if expected.unit == Unit::Currency {
            let current = input.currency.as_deref().ok_or_else(|| "An explicit currency is required.".to_string())?;
            if let Some(previous) = &currency {
                if previous != current {
                    return Err("Currency mismatch: FRIDAY will not convert currencies implicitly.".into());
                }
            } else { currency = Some(current.to_string()); }
        }
        let decimal = Decimal::parse(input.value.as_deref().unwrap_or_default())?;
        trace.push(format!("Input {} = {} {}", expected.name, decimal.format(), unit_name(&expected.unit)));
        values.insert(expected.name, decimal);
    }
    if !missing.is_empty() {
        return Ok(result_incomplete(definition, missing, trace));
    }
    let get = |name: &str| values.get(name).copied()
        .ok_or_else(|| "A required formula value is unavailable.".to_string());
    let output = match definition.id {
        "accounting.net_sales" => {
            let gross = get("gross_sales")?;
            let discounts = get("discounts")?;
            let returns = get("sales_returns")?;
            trace.push("Calculation: gross_sales - discounts - sales_returns".into());
            gross.checked_sub(discounts)?.checked_sub(returns)?
        }
        "accounting.gross_profit" => {
            trace.push("Calculation: net_sales - cost_of_goods_sold".into());
            get("net_sales")?.checked_sub(get("cost_of_goods_sold")?)?
        }
        "accounting.gross_margin" => {
            let sales = get("net_sales")?;
            if sales.micros == 0 {
                return Err("Gross margin is undefined when net sales are zero.".into());
            }
            trace.push("Calculation: ((net_sales - cost_of_goods_sold) / net_sales) * 100".into());
            get("net_sales")?.checked_sub(get("cost_of_goods_sold")?)?
                .checked_mul(Decimal { micros: 100 * DECIMAL_FACTOR })?
                .checked_div(sales)?
        }
        _ => return Err("The registered formula has no deterministic implementation.".into()),
    };
    trace.push(format!("Result = {} {}", output.format(), unit_name(&definition.output_unit)));
    Ok(CalculationResult {
        formula_id: definition.id.into(), formula_version: definition.version, status: "calculated".into(),
        output_value: Some(output.format()), output_unit: unit_name(&definition.output_unit).into(),
        currency: if definition.output_unit == Unit::Currency { currency } else { None },
        missing_inputs: Vec::new(), trace, warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn currency(name: &str, value: Option<&str>, state: &str, code: &str) -> CalculationInput {
        CalculationInput { name: name.into(), state: ValueState::parse(state).unwrap(),
            value: value.map(str::to_string), unit: Unit::Currency, currency: Some(code.into()) }
    }

    #[test]
    fn fixed_point_decimal_preserves_money_and_rounds_only_division() {
        let a = Decimal::parse("0.10").unwrap();
        let b = Decimal::parse("0.20").unwrap();
        assert_eq!(a.checked_add(b).unwrap().format(), "0.3");
        assert_eq!(Decimal::parse("19.99").unwrap().checked_mul(Decimal::parse("3").unwrap()).unwrap().format(), "59.97");
        assert_eq!(Decimal::parse("1").unwrap().checked_div(Decimal::parse("8").unwrap()).unwrap().format(), "0.125");
        assert_eq!(Decimal::parse("-1").unwrap().checked_div(Decimal::parse("6").unwrap()).unwrap().format(), "-0.166667");
    }

    #[test]
    fn decimal_parser_rejects_exponents_extra_precision_and_invalid_tokens() {
        assert!(Decimal::parse("1e6").is_err());
        assert!(Decimal::parse("1.0000001").is_err());
        assert!(Decimal::parse("NaN").is_err());
        assert!(Decimal::parse("1.2.3").is_err());
        assert!(Decimal::parse("").is_err());
    }

    #[test]
    fn decimal_operations_detect_overflow_and_division_by_zero() {
        let huge = Decimal::parse("170000000000000000000000000000000").unwrap();
        assert!(huge.checked_add(huge).is_err());
        assert!(Decimal::parse("1").unwrap().checked_div(Decimal::parse("0").unwrap()).is_err());
    }

    #[test]
    fn formula_registry_is_versioned_and_contains_only_reviewed_ids() {
        let registry = formula_registry();
        assert_eq!(registry.len(), 3);
        assert!(registry.iter().all(|item| item.version == 1));
        assert!(formula("accounting.net_sales").is_ok());
        assert!(formula("accounting.arbitrary_expression").is_err());
    }

    #[test]
    fn net_sales_uses_explicit_inputs_and_returns_trace() {
        let result = calculate("accounting.net_sales", vec![
            currency("gross_sales", Some("1000.00"), "provided", "PKR"),
            currency("discounts", Some("50.00"), "provided", "PKR"),
            currency("sales_returns", Some("25.00"), "provided", "PKR"),
        ]).unwrap();
        assert_eq!(result.status, "calculated");
        assert_eq!(result.output_value.as_deref(), Some("925"));
        assert_eq!(result.currency.as_deref(), Some("PKR"));
        assert!(result.trace.iter().any(|line| line.contains("gross_sales - discounts - sales_returns")));
    }

    #[test]
    fn gross_profit_and_margin_use_reviewed_formulas() {
        let profit = calculate("accounting.gross_profit", vec![
            currency("net_sales", Some("925"), "provided", "PKR"),
            currency("cost_of_goods_sold", Some("600"), "provided", "PKR"),
        ]).unwrap();
        assert_eq!(profit.output_value.as_deref(), Some("325"));
        let margin = calculate("accounting.gross_margin", vec![
            currency("net_sales", Some("925"), "provided", "PKR"),
            currency("cost_of_goods_sold", Some("600"), "provided", "PKR"),
        ]).unwrap();
        assert_eq!(margin.output_value.as_deref(), Some("35.135135"));
        assert_eq!(margin.output_unit, "percent");
        assert!(margin.currency.is_none());
    }

    #[test]
    fn missing_unknown_and_not_applicable_are_not_silently_zeroed() {
        for state in ["missing", "unknown", "not_applicable"] {
            let result = calculate("accounting.net_sales", vec![
                currency("gross_sales", Some("100"), "provided", "USD"),
                currency("discounts", None, state, "USD"),
                currency("sales_returns", Some("0"), "provided", "USD"),
            ]).unwrap();
            assert_eq!(result.status, "incomplete");
            assert!(result.output_value.is_none());
            assert!(!result.missing_inputs.is_empty());
        }
    }

    #[test]
    fn formula_rejects_currency_mismatch_duplicate_and_unexpected_inputs() {
        assert!(calculate("accounting.net_sales", vec![
            currency("gross_sales", Some("100"), "provided", "USD"),
            currency("discounts", Some("1"), "provided", "EUR"),
            currency("sales_returns", Some("0"), "provided", "USD"),
        ]).is_err());
        let duplicate = currency("gross_sales", Some("100"), "provided", "USD");
        assert!(calculate("accounting.net_sales", vec![
            duplicate.clone(), duplicate, currency("discounts", Some("1"), "provided", "USD"),
            currency("sales_returns", Some("0"), "provided", "USD"),
        ]).is_err());
        assert!(calculate("accounting.net_sales", vec![
            currency("gross_sales", Some("100"), "provided", "USD"),
            currency("discounts", Some("1"), "provided", "USD"),
            currency("sales_returns", Some("0"), "provided", "USD"),
            currency("tax", Some("5"), "provided", "USD"),
        ]).is_err());
    }

    #[test]
    fn gross_margin_rejects_zero_net_sales() {
        assert!(calculate("accounting.gross_margin", vec![
            currency("net_sales", Some("0"), "provided", "USD"),
            currency("cost_of_goods_sold", Some("10"), "provided", "USD"),
        ]).is_err());
    }

    #[test]
    fn invalid_input_state_value_pair_and_currency_are_rejected() {
        assert!(calculate("accounting.net_sales", vec![
            currency("gross_sales", Some("100"), "unknown", "USD"),
            currency("discounts", Some("1"), "provided", "USD"),
            currency("sales_returns", Some("0"), "provided", "USD"),
        ]).is_err());
        assert!(calculate("accounting.net_sales", vec![
            currency("gross_sales", Some("100"), "provided", "usd"),
            currency("discounts", Some("1"), "provided", "USD"),
            currency("sales_returns", Some("0"), "provided", "USD"),
        ]).is_err());
    }
}
