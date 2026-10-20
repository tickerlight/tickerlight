//! Models trained elsewhere, loaded from ONNX files, run as strategies: at each close a model reads the numbers the
//! program describes the bar with ([`bar_features`], written out for training by [`features_csv`]) and gives the
//! chance of a higher next close; the strategy goes with it as the model trained inside does ([`go_with`]).
//!
//! A model takes a float tensor of [`FEATURES`] numbers (shape `[1, 6]`, or `[n, 6]` with any `n`) and gives,
//! as its first float output, the chance of a higher close (one number from 0 to 1) or the chances of a lower and
//! a higher one (two numbers, as a classifier's probabilities).
//!
//! [`features_csv`]: tickerlight_core::builtin::features_csv

use std::path::Path;

use tickerlight_core::builtin::{FEATURES, Setting, Strategy, StrategyCtx, bar_features, go_with};
use tract_onnx::prelude::*;
use tract_onnx::tract_core::plan::SimpleState;
use tract_onnx::tract_hir::infer::Factoid;

/// A model read from an ONNX file, trading on the chance it gives.
#[derive(Debug)]
pub struct OnnxModel {
    /// Its file name, without the extension.
    name: String,
    /// How sure it has to be, percent: long at this chance of a higher close or more, short at as much of a lower.
    pub sure: f64,
    state: SimpleState<TypedFact, Box<dyn TypedOp>>,
    /// Which of its outputs holds the chance, and whether it holds two (lower, higher).
    output: usize,
    two: bool,
    /// Why it stopped in this run: it trades no more until the next.
    stopped: Option<String>,
}

impl OnnxModel {
    /// The model in `bytes` (an ONNX file) named `name`; what is wrong with it otherwise.
    pub fn new(name: &str, bytes: &[u8]) -> Result<Self, String> {
        let model = tract_onnx::onnx().model_for_read(&mut &bytes[..]).map_err(why)?;
        let input = model.input_fact(0).map_err(why)?;
        let width = input.shape.dims().last().and_then(|d| match d.concretize() {
            Some(TDim::Val(width)) => Some(width),
            _ => None,
        });
        if let Some(width) = width
            && width != FEATURES as i64
        {
            return Err(format!("it takes {width} numbers a bar, the bars give {FEATURES}"));
        }
        let plan = model
            .with_input_fact(0, f32::fact([1, FEATURES]).into())
            .and_then(|m| m.into_optimized())
            .and_then(|m| m.into_runnable_with_options(&Default::default()))
            .map_err(why)?;
        let mut state = plan.spawn().map_err(why)?;
        // Where the chance is: the first float output of one number or two.
        let outputs = state.run(tvec!(input_of(&[0.0; FEATURES])?.into())).map_err(why)?;
        let (output, two) = outputs
            .iter()
            .position(|o| o.datum_type().is_float() && (1..=2).contains(&o.len()))
            .map(|i| (i, outputs[i].len() == 2))
            .ok_or("it gives no chance: one number, or two (lower, higher)")?;
        Ok(Self {
            name: name.to_string(),
            sure: 55.0,
            state,
            output,
            two,
            stopped: None,
        })
    }

    /// Why it stopped in the last run, if it did.
    pub fn stopped(&self) -> Option<&str> {
        self.stopped.as_deref()
    }

    /// The chance of a higher next close it gives for a bar's `features`.
    fn chance(&mut self, features: &[f64; FEATURES]) -> Result<f64, String> {
        let outputs = self.state.run(tvec!(input_of(features)?.into())).map_err(why)?;
        let out = outputs[self.output].cast_to::<f64>().map_err(why)?;
        let values = out.to_plain_array_view::<f64>().map_err(why)?;
        let mut values = values.iter().copied();
        let first = values.next();
        Ok(if self.two { values.next() } else { first }.unwrap_or(f64::NAN))
    }
}

/// The input tensor of a bar's features.
fn input_of(features: &[f64; FEATURES]) -> Result<Tensor, String> {
    let values: Vec<f32> = features.iter().map(|&x| x as f32).collect();
    Tensor::from_shape(&[1, FEATURES], &values).map_err(why)
}

/// What went wrong, with what it came from.
fn why(error: TractError) -> String {
    format!("{error:#}")
}

impl Strategy for OnnxModel {
    fn name(&self) -> String {
        let name = format!("Model {} {}", self.name, self.sure);
        match &self.stopped {
            Some(why) => format!("{name} (stopped: {why})"),
            None => name,
        }
    }

    fn settings(&mut self) -> Vec<Setting<'_>> {
        vec![Setting {
            name: "Sure %",
            value: &mut self.sure,
        }]
    }

    fn another(&self) -> Option<Box<dyn Strategy>> {
        Some(Box::new(Self {
            name: self.name.clone(),
            sure: self.sure,
            state: self.state.plan().spawn().ok()?,
            output: self.output,
            two: self.two,
            stopped: None,
        }))
    }

    fn start(&mut self) {
        self.stopped = None;
    }

    fn on_bar(&mut self, ctx: &mut StrategyCtx<'_>) {
        let features = bar_features(ctx);
        if self.stopped.is_some() || !features.iter().all(|x| x.is_finite()) {
            return;
        }
        match self.chance(&features) {
            Ok(up) if up.is_finite() => go_with(ctx, up, self.sure),
            Ok(_) => {}
            Err(why) => self.stopped = Some(why),
        }
    }
}

/// The models in the `.onnx` files of `folder`, by file name, and what is wrong with those that do not load. No
/// folder is no models.
pub fn load_folder(folder: &Path) -> (Vec<OnnxModel>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return (Vec::new(), Vec::new());
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "onnx"))
        .collect();
    paths.sort();
    let (mut models, mut problems) = (Vec::new(), Vec::new());
    for path in paths {
        let file = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        match std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| OnnxModel::new(&file, &bytes))
        {
            Ok(model) => models.push(model),
            Err(why) => problems.push(format!("{file}: {why}")),
        }
    }
    (models, problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use tickerlight_core::builtin::{Account, backtest};
    use tickerlight_core::{Candle, Candles, Time};
    use tract_onnx::pb;

    fn value(name: &str, dims: &[i64]) -> pb::ValueInfoProto {
        use pb::tensor_shape_proto::{Dimension, dimension};
        let dim = |d: i64| Dimension {
            value: Some(if d < 0 {
                dimension::Value::DimParam("n".into())
            } else {
                dimension::Value::DimValue(d)
            }),
            ..Default::default()
        };
        let tensor = pb::type_proto::Tensor {
            elem_type: 1,
            shape: Some(pb::TensorShapeProto {
                dim: dims.iter().map(|&d| dim(d)).collect(),
            }),
        };
        pb::ValueInfoProto {
            name: name.into(),
            r#type: Some(pb::TypeProto {
                value: Some(pb::type_proto::Value::TensorType(tensor)),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// An ONNX model of a batch of bars of `weights.len()` numbers: the chance of a higher close is the sigmoid of
    /// `weights` times them plus `bias`; with `two`, it gives the chances of a lower and a higher close.
    fn model(weights: &[f32], bias: f32, two: bool) -> Vec<u8> {
        let tensor = |name: &str, dims: Vec<i64>, values: Vec<f32>| pb::TensorProto {
            name: name.into(),
            dims,
            data_type: 1,
            float_data: values,
            ..Default::default()
        };
        let node = |op: &str, inputs: &[&str], output: &str| pb::NodeProto {
            op_type: op.into(),
            name: output.into(),
            input: inputs.iter().map(|s| s.to_string()).collect(),
            output: vec![output.into()],
            ..Default::default()
        };
        let mut nodes = vec![
            node("MatMul", &["x", "w"], "xw"),
            node("Add", &["xw", "b"], "z"),
            node("Sigmoid", &["z"], "up"),
        ];
        if two {
            nodes.push(node("Sub", &["one", "up"], "down"));
            let mut concat = node("Concat", &["down", "up"], "chances");
            concat.attribute = vec![pb::AttributeProto {
                name: "axis".into(),
                r#type: pb::attribute_proto::AttributeType::Int as i32,
                i: 1,
                ..Default::default()
            }];
            nodes.push(concat);
        }
        let width = weights.len() as i64;
        let graph = pb::GraphProto {
            name: "test".into(),
            node: nodes,
            initializer: vec![
                tensor("w", vec![width, 1], weights.to_vec()),
                tensor("b", vec![1], vec![bias]),
                tensor("one", vec![1], vec![1.0]),
            ],
            input: vec![value("x", &[-1, width])],
            output: vec![if two {
                value("chances", &[-1, 2])
            } else {
                value("up", &[-1, 1])
            }],
            ..Default::default()
        };
        pb::ModelProto {
            ir_version: 8,
            opset_import: vec![pb::OperatorSetIdProto {
                domain: String::new(),
                version: 13,
            }],
            graph: Some(graph),
            ..Default::default()
        }
        .encode_to_vec()
    }

    fn bars(closes: &[f64]) -> Candles {
        let mut candles = Candles::new();
        for (i, &c) in closes.iter().enumerate() {
            let open = if i > 0 { closes[i - 1] } else { c };
            let candle = Candle {
                time: Time(i as i64 * 60_000),
                open,
                high: c.max(open) + 0.5,
                low: c.min(open) - 0.5,
                close: c,
                volume: 1.0,
            };
            candles.push(candle).unwrap();
        }
        candles
    }

    /// Against the last bar: the more it rose, the lower the chance of a higher close.
    const AGAINST: [f32; FEATURES] = [-200.0, 0.0, 0.0, 0.0, 0.0, 0.0];

    #[test]
    fn a_model_reads_each_bars_features_and_trades_on_the_chance_it_gives() {
        // Every rise is followed by a fall and the other way round, over a slow drift.
        let closes: Vec<f64> = (0..300)
            .map(|i| 100.0 + i as f64 * 0.01 + if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let data = bars(&closes);
        for two in [false, true] {
            let mut against = OnnxModel::new("against", &model(&AGAINST, 0.0, two)).unwrap();
            assert_eq!(against.name(), "Model against 55");
            let run = backtest(&mut against, data.view(), Account::default());
            let fills = run.account.fills();
            assert!(fills.len() > 100, "{}", fills.len());
            // Each order goes against the bar it was placed at: it fills at the next open.
            for fill in fills {
                let k = (fill.time.0 / 60_000) as usize;
                assert_eq!(
                    fill.qty < 0.0,
                    closes[k - 1] > closes[k - 2],
                    "bar {k}, two outputs: {two}"
                );
            }

            // A rise or fall of about 2% gives it a chance of about 98% the other way: not sure enough at 99%.
            against.sure = 99.0;
            let run = backtest(&mut against, data.view(), Account::default());
            assert!(run.account.fills().is_empty());
        }
    }

    #[test]
    fn a_model_that_reads_other_numbers_or_is_no_model_is_refused_with_why() {
        let narrow = OnnxModel::new("narrow", &model(&[1.0; 4], 0.0, false)).unwrap_err();
        assert_eq!(narrow, "it takes 4 numbers a bar, the bars give 6");
        assert!(OnnxModel::new("noise", b"not a model").is_err());

        let folder = std::env::temp_dir().join(format!("tickerlight-onnx-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("good.onnx"), model(&AGAINST, 0.0, true)).unwrap();
        std::fs::write(folder.join("bad.onnx"), b"not a model").unwrap();
        std::fs::write(folder.join("notes.txt"), b"not read").unwrap();
        let (models, problems) = load_folder(&folder);
        std::fs::remove_dir_all(&folder).unwrap();
        let names: Vec<String> = models.iter().map(|m| m.name()).collect();
        assert_eq!(names, ["Model good 55"]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("bad: "), "{problems:?}");
        assert!(load_folder(&folder).0.is_empty(), "no folder, no models");
    }
}
