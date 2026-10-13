use super::*;
use crate::model::{
    CompiledModelDesc, FlexController, FlexDescriptor, FlexOpData, FlexOpKind, ResolvedFlexOp,
    ResolvedFlexRule,
};

#[derive(Debug, Clone, PartialEq)]
pub struct ExplicitControl {
    pub name: String,
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuleKind {
    PassThrough,
    Expression(String),
    LocalVar,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExplicitRule {
    pub name: String,
    pub kind: RuleKind,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExplicitRules {
    pub controls: Vec<ExplicitControl>,
    pub rules: Vec<ExplicitRule>,
}

pub(super) fn read_targets(document: &Document, operator: &Node) -> Result<Option<ExplicitRules>> {
    let targets = elements(operator, "targets")?;
    if targets.iter().any(|&id| {
        document
            .nodes
            .get(id)
            .is_none_or(|n| n.class != "DmeFlexRules")
    }) {
        return Ok(None);
    }
    let mut result = ExplicitRules::default();
    let mut controls = HashSet::new();
    for id in elements(operator, "controls")? {
        let control = node(document, id)?;
        if control.name.is_empty() || !controls.insert(control.name.to_ascii_lowercase()) {
            return Err("DMX explicit controls have an empty or duplicate name".into());
        }
        let min = number(control, "flexMin", 0.)?;
        let max = number(control, "flexMax", 1.)?;
        if min >= max {
            return Err(format!(
                "DMX explicit control {} has an invalid range",
                control.name
            ));
        }
        result.controls.push(ExplicitControl {
            name: control.name.clone(),
            min,
            max,
        });
    }
    let mut names = HashSet::new();
    for id in targets {
        for id in elements(node(document, id)?, "deltaStates")? {
            let rule = node(document, id)?;
            if rule.name.is_empty() || !names.insert(rule.name.to_ascii_lowercase()) {
                return Err("DMX explicit flex rules have an empty or duplicate name".into());
            }
            let kind = match rule.class.as_str() {
                "DmeFlexRulePassThrough" => RuleKind::PassThrough,
                "DmeFlexRuleExpression" => RuleKind::Expression(string(rule, "expr", "")?),
                "DmeFlexRuleLocalVar" => RuleKind::LocalVar,
                class => {
                    return Err(format!(
                        "DMX explicit flex rule {} has unsupported type {class}",
                        rule.name
                    ));
                }
            };
            result.rules.push(ExplicitRule {
                name: rule.name.clone(),
                kind,
            });
        }
    }
    if result.controls.len() > 96 || result.rules.len() > 1024 {
        return Err(
            "DMX explicit flex rules exceed Source's controller or descriptor limits".into(),
        );
    }
    Ok(Some(result))
}

pub(super) fn register(compiled: &mut CompiledModelDesc, rules: &ExplicitRules) -> Result<()> {
    for control in &rules.controls {
        if let Some(existing) = compiled
            .desc
            .flex_controllers
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(&control.name))
        {
            if existing.kind != "default"
                || existing.min != control.min
                || existing.max != control.max
            {
                return Err(format!(
                    "DMX explicit control {} conflicts with an existing controller",
                    control.name
                ));
            }
        } else {
            if compiled.desc.flex_controllers.len() >= 96 {
                return Err("DMX model exceeds Source's 96 flex controllers".into());
            }
            compiled.desc.flex_controllers.push(FlexController {
                name: control.name.clone(),
                kind: "default".into(),
                min: control.min,
                max: control.max,
            });
        }
    }
    for rule in &rules.rules {
        if !compiled
            .desc
            .flex_descriptors
            .iter()
            .any(|d| d.name.eq_ignore_ascii_case(&rule.name))
        {
            if compiled.desc.flex_descriptors.len() >= 1024 {
                return Err("DMX model exceeds Source's 1024 flex descriptors".into());
            }
            compiled.desc.flex_descriptors.push(FlexDescriptor {
                name: rule.name.clone(),
            });
        }
    }
    Ok(())
}

fn lower(compiled: &CompiledModelDesc, rules: &ExplicitRules) -> Result<Vec<ResolvedFlexRule>> {
    let controllers: Vec<String> = compiled
        .desc
        .flex_controllers
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let descriptors: Vec<String> = compiled
        .desc
        .flex_descriptors
        .iter()
        .map(|d| d.name.clone())
        .collect();
    let mut out = Vec::new();
    for rule in &rules.rules {
        let expression = match &rule.kind {
            RuleKind::PassThrough => &rule.name,
            RuleKind::Expression(expression) => expression,
            RuleKind::LocalVar => continue,
        };
        if expression.contains(['\n', '\r']) {
            return Err(format!(
                "DMX flex rule {} contains multiple expression lines",
                rule.name
            ));
        }
        let mut lexer = crate::qc::lexer::Lexer::new(std::path::PathBuf::new());
        lexer.load_from_memory(
            &format!("= {expression}\n"),
            &format!("DMX flex {}", rule.name),
        );
        let parsed =
            crate::qc::flexrule::parse_flexrule(&mut lexer, &rule.name, &controllers, &descriptors)
                .map_err(|e| e.to_string())?;
        let flex = descriptors
            .iter()
            .position(|d| d.eq_ignore_ascii_case(&rule.name))
            .ok_or_else(|| format!("DMX descriptor {} is missing", rule.name))?
            as i32;
        let mut depth = 0usize;
        let mut ops = Vec::with_capacity(parsed.ops.len());
        for op in parsed.ops {
            let d = match op.op {
                FlexOpKind::Const => {
                    let value = op.value.ok_or("DMX expression is missing a constant")?;
                    if !value.is_finite() {
                        return Err(format!(
                            "DMX flex rule {} has a nonfinite constant",
                            rule.name
                        ));
                    }
                    depth += 1;
                    FlexOpData::Value(value)
                }
                FlexOpKind::Fetch1 | FlexOpKind::Fetch2 => {
                    let (names, name) = if op.op == FlexOpKind::Fetch1 {
                        (&controllers, op.controller.as_ref())
                    } else {
                        (&descriptors, op.flexdesc.as_ref())
                    };
                    let index = name
                        .and_then(|name| names.iter().position(|n| n.eq_ignore_ascii_case(name)))
                        .ok_or(
                            "DMX expression references an unregistered controller or descriptor",
                        )?;
                    depth += 1;
                    FlexOpData::Index(index as i32)
                }
                FlexOpKind::Neg => {
                    if depth == 0 {
                        return Err(format!(
                            "DMX flex rule {} has an invalid expression stack",
                            rule.name
                        ));
                    }
                    FlexOpData::None
                }
                FlexOpKind::Add
                | FlexOpKind::Sub
                | FlexOpKind::Mul
                | FlexOpKind::Div
                | FlexOpKind::Min
                | FlexOpKind::Max => {
                    if depth < 2 {
                        return Err(format!(
                            "DMX flex rule {} has an invalid expression stack",
                            rule.name
                        ));
                    }
                    depth -= 1;
                    FlexOpData::None
                }
                _ => {
                    return Err(format!(
                        "DMX flex rule {} contains an incomplete expression",
                        rule.name
                    ));
                }
            };
            if depth > 32 {
                return Err(format!(
                    "DMX flex rule {} exceeds Source's 32-value stack",
                    rule.name
                ));
            }
            ops.push(ResolvedFlexOp {
                op: op.op.code(),
                d,
            });
        }
        if depth != 1 {
            return Err(format!(
                "DMX flex rule {} does not produce one result",
                rule.name
            ));
        }
        out.push(ResolvedFlexRule { flex, ops });
    }
    let producers: HashMap<i32, usize> = out.iter().enumerate().map(|(i, r)| (r.flex, i)).collect();
    let mut states = vec![0u8; out.len()];
    let mut order = Vec::with_capacity(out.len());
    fn visit(
        index: usize,
        rules: &[ResolvedFlexRule],
        producers: &HashMap<i32, usize>,
        states: &mut [u8],
        order: &mut Vec<usize>,
        depth: usize,
    ) -> Result<()> {
        if depth > 128 {
            return Err("DMX flex-rule dependency depth exceeds 128".into());
        }
        if states[index] == 2 {
            return Ok(());
        }
        if states[index] == 1 {
            return Err("DMX flex rules contain a cyclic descriptor dependency".into());
        }
        states[index] = 1;
        for op in &rules[index].ops {
            if let (3, FlexOpData::Index(id)) = (op.op, op.d)
                && let Some(&dependency) = producers.get(&id)
            {
                visit(dependency, rules, producers, states, order, depth + 1)?;
            }
        }
        states[index] = 2;
        order.push(index);
        Ok(())
    }
    for index in 0..out.len() {
        visit(index, &out, &producers, &mut states, &mut order, 0)?;
    }
    Ok(order.into_iter().map(|i| out[i].clone()).collect())
}

pub(super) fn resolve(compiled: &mut CompiledModelDesc, rules: &ExplicitRules) -> Result<()> {
    let rules = lower(compiled, rules)?;
    compiled.resolved_flex_rules.extend(rules);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn explicit_rules() -> ExplicitRules {
        let document = data::parse(
            br#"<!-- dmx encoding keyvalues2 1 format model 18 -->
"DmeCombinationOperator" {
    "name" "string" "operator"
    "controls" "element_array" [
        "DmeCombinationInputControl" {
            "name" "string" "smile"
            "flexMin" "float" "-1"
            "flexMax" "float" "1"
            "stereo" "bool" "1"
        }
    ]
    "targets" "element_array" [
        "DmeFlexRules" {
            "name" "string" "rules"
            "deltaStates" "element_array" [
                "DmeFlexRulePassThrough" { "name" "string" "smile" }
                "DmeFlexRuleExpression" {
                    "name" "string" "jaw"
                    "expr" "string" "max(0, %smile * 2 - 0.25)"
                }
                "DmeFlexRuleLocalVar" { "name" "string" "scratch" }
            ]
        }
    ]
}
"#,
        )
        .unwrap();
        read_targets(&document, &document.nodes[document.root])
            .unwrap()
            .unwrap()
    }

    fn model() -> CompiledModelDesc {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mdlc-explicit-flex-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mesh.obj"), "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
        let desc =
            crate::qc::parse_qc_str("$modelname flex.mdl\n$body mesh mesh.obj\n", &dir).unwrap();
        let model = crate::compile::compile(&desc, &dir).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        model
    }

    fn read_i32(bytes: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_f32(bytes: &[u8], offset: usize) -> f32 {
        f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn cstr(bytes: &[u8], offset: usize) -> &str {
        let length = bytes[offset..].iter().position(|&b| b == 0).unwrap();
        std::str::from_utf8(&bytes[offset..offset + length]).unwrap()
    }

    #[test]
    fn explicit_dmx_rules_preserve_plain_controller_ranges_and_vm_operands_in_mdl() {
        let rules = explicit_rules();
        let mut compiled = model();
        compiled.bodyparts[0].models[0].dmx = Some(Metadata {
            has_combination: true,
            explicit_rules: Some(rules),
            ..Default::default()
        });
        super::super::register_flexes(&mut compiled).unwrap();
        super::super::resolve_flexes(&mut compiled).unwrap();
        let bytes = crate::mdl_writer::write_mdl(&compiled).unwrap().bytes;
        assert_eq!(read_i32(&bytes, 0x10c), 1);
        let controller = read_i32(&bytes, 0x110) as usize;
        assert_eq!(
            cstr(&bytes, controller + read_i32(&bytes, controller) as usize),
            "default"
        );
        assert_eq!(
            cstr(
                &bytes,
                controller + read_i32(&bytes, controller + 4) as usize
            ),
            "smile"
        );
        assert_eq!(
            [
                read_f32(&bytes, controller + 12),
                read_f32(&bytes, controller + 16)
            ],
            [-1., 1.]
        );
        assert_eq!(read_i32(&bytes, 0x104), 3);
        assert_eq!(read_i32(&bytes, 0x114), 2);
        let descriptors = read_i32(&bytes, 0x108) as usize;
        assert_eq!(
            cstr(
                &bytes,
                descriptors + 8 + read_i32(&bytes, descriptors + 8) as usize
            ),
            "scratch"
        );
        let first_rule = read_i32(&bytes, 0x118) as usize;
        let first_op = first_rule + read_i32(&bytes, first_rule + 8) as usize;
        assert_eq!(read_i32(&bytes, first_rule), 0);
        assert_eq!(read_i32(&bytes, first_rule + 4), 1);
        assert_eq!(
            [read_i32(&bytes, first_op), read_i32(&bytes, first_op + 4)],
            [2, 0]
        );
        let second_rule = first_rule + 12;
        assert_eq!(read_i32(&bytes, second_rule), 1);
        let second_op = second_rule + read_i32(&bytes, second_rule + 8) as usize;
        let count = read_i32(&bytes, second_rule + 4) as usize;
        let mut values = [0.; 3];
        for smile in [-1., 0., 0.5, 1.] {
            values[0] = smile;
            let mut stack = Vec::<f32>::new();
            for i in 0..count {
                let base = second_op + i * 8;
                match read_i32(&bytes, base) {
                    1 => stack.push(read_f32(&bytes, base + 4)),
                    3 => stack.push(values[read_i32(&bytes, base + 4) as usize]),
                    code @ (4..=7 | 13..=14) => {
                        let b = stack.pop().unwrap();
                        let a = stack.pop().unwrap();
                        stack.push(match code {
                            4 => a + b,
                            5 => a - b,
                            6 => a * b,
                            7 => a / b,
                            13 => a.max(b),
                            14 => a.min(b),
                            _ => unreachable!(),
                        });
                    }
                    code => panic!("unexpected serialized opcode {code}"),
                }
            }
            assert_eq!(stack, vec![(smile * 2. - 0.25).max(0.)]);
        }
    }

    #[test]
    fn explicit_rules_resolve_dependencies_and_reject_cycles_unknown_inputs_and_bad_stacks() {
        let mut rules = explicit_rules();
        rules.rules.swap(0, 1);
        let mut compiled = model();
        register(&mut compiled, &rules).unwrap();
        let resolved = lower(&compiled, &rules).unwrap();
        assert_eq!(resolved.iter().map(|r| r.flex).collect::<Vec<_>>(), [1, 0]);
        for (expression, message) in [
            ("%jaw", "cyclic"),
            ("missing", "unknown controller"),
            ("smile +", "stack"),
            ("", "one result"),
            ("99999999999999999999999999999999999999999", "nonfinite"),
        ] {
            rules.rules[0].kind = RuleKind::Expression(expression.into());
            assert!(
                lower(&compiled, &rules).unwrap_err().contains(message),
                "{expression}"
            );
        }
    }
}
