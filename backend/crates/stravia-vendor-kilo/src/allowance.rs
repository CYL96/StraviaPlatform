use super::*;

#[derive(Default)]
struct Decimal {
    coefficient: i128,
    scale: u32,
}
impl Decimal {
    fn parse(value: &Value) -> Result<Self, PluginError> {
        if !value.is_number() {
            return Err(fail(
                ErrorKind::Invalid,
                "Kilo usage cost must be numeric microdollars",
            ));
        }
        let text = value.to_string();
        let (mantissa, exponent) = text
            .split_once(['e', 'E'])
            .map_or((text.as_str(), 0), |(m, e)| {
                (m, e.parse::<i32>().unwrap_or(i32::MAX))
            });
        let fractional = mantissa.split_once('.').map_or(0, |(_, v)| v.len() as i32);
        let digits = mantissa.replace('.', "");
        let coefficient = digits
            .parse::<i128>()
            .map_err(|_| fail(ErrorKind::Invalid, "Kilo usage cost exceeds decimal range"))?;
        let scale = fractional
            .checked_sub(exponent)
            .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo usage exponent exceeds range"))?;
        if coefficient < 0 {
            return Err(fail(ErrorKind::Invalid, "Kilo usage cost is negative"));
        }
        if scale < 0 {
            let multiplier = 10_i128
                .checked_pow(scale.unsigned_abs())
                .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo usage exponent exceeds range"))?;
            Ok(Self {
                coefficient: coefficient
                    .checked_mul(multiplier)
                    .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo usage cost overflow"))?,
                scale: 0,
            })
        } else {
            Ok(Self {
                coefficient,
                scale: scale as u32,
            })
        }
    }
    fn add(&mut self, other: &Self) -> Result<(), PluginError> {
        let scale = self.scale.max(other.scale);
        let align = |value: i128, delta: u32| {
            10_i128
                .checked_pow(delta)
                .and_then(|p| value.checked_mul(p))
                .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo usage decimal overflow"))
        };
        self.coefficient = align(self.coefficient, scale - self.scale)?
            .checked_add(align(other.coefficient, scale - other.scale)?)
            .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo usage sum overflow"))?;
        self.scale = scale;
        Ok(())
    }
    fn usd(&self) -> Result<String, PluginError> {
        let scale = self
            .scale
            .checked_add(6)
            .filter(|v| *v <= 128)
            .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo usage decimal scale exceeds range"))?
            as usize;
        let mut digits = self.coefficient.to_string();
        if digits.len() <= scale {
            digits = format!("{}{}", "0".repeat(scale + 1 - digits.len()), digits);
        }
        digits.insert(digits.len() - scale, '.');
        while digits.ends_with('0') {
            digits.pop();
        }
        if digits.ends_with('.') {
            digits.pop();
        }
        Ok(digits)
    }
}
fn item(key: String, label: String, cost: &Decimal) -> Result<AllowanceItem, PluginError> {
    Ok(AllowanceItem {
        key,
        label,
        kind: "usage".into(),
        used: Some(AllowanceAmount {
            value: cost.usd()?,
            unit: "USD".into(),
            currency: Some("USD".into()),
        }),
        remaining: None,
        limit: None,
        used_percent: None,
        window_seconds: None,
        resets_at_unix_ms: None,
        condition: None,
    })
}
pub(super) fn parse(value: Value) -> Result<AllowanceResponse, PluginError> {
    let rows = value
        .get("usage")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            fail(
                ErrorKind::Invalid,
                "Kilo usage response missing usage array",
            )
        })?;
    let mut total = Decimal::default();
    let mut grouped: BTreeMap<String, Decimal> = BTreeMap::new();
    for row in rows {
        let date = row
            .get("date")
            .and_then(Value::as_str)
            .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo usage date missing"))?;
        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|_| fail(ErrorKind::Invalid, "Invalid Kilo usage date"))?;
        let cost = Decimal::parse(
            row.get("total_cost")
                .ok_or_else(|| fail(ErrorKind::Invalid, "Kilo usage cost missing"))?,
        )?;
        total.add(&cost)?;
        if let Some(model) = row.get("model").and_then(Value::as_str) {
            grouped.entry(model.into()).or_default().add(&cost)?;
        }
    }
    let models = grouped
        .into_iter()
        .map(|(model, cost)| {
            Ok(ModelAllowance {
                model,
                allowances: vec![item("usage:all".into(), "All-time usage".into(), &cost)?],
            })
        })
        .collect::<Result<Vec<_>, PluginError>>()?;
    Ok(AllowanceResponse {
        allowances: vec![item("usage:all".into(), "All-time usage".into(), &total)?],
        models,
        plan_label: None,
    })
}
pub(super) fn execute(
    host: &GuestHost,
    provider: &ProviderSnapshot,
) -> Result<AllowanceResponse, PluginError> {
    if key(provider)?.is_none() {
        return Err(fail(
            ErrorKind::Unsupported,
            "Allowance is unavailable for anonymous Kilo access",
        ));
    }
    let view = escape(organization(provider).unwrap_or("personal"));
    // Balance has no confirmed unit; it must not become a USD allowance.
    parse(http_json(
        host,
        provider,
        "GET",
        &format!("/api/profile/usage?period=all&viewType={view}&groupByModel=true"),
    )?)
}
