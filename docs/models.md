# Training a model elsewhere

A model trained in Python (or anywhere that writes ONNX) runs in Tickerlight as a strategy: at each close it reads
the numbers the program describes the bar with and gives the chance of a higher next close. The strategy goes long
with all of the equity when that chance is at least its "Sure %" setting (55 to start), and short when the chance
of a lower close is; otherwise it keeps what it holds. It is backtested, replayed, tested apart, walked forward and
traded on paper like any other strategy.

## 1. Write the bars

Open the symbol and timeframe to train on. Strategy, then "Write the bars for training": the program writes
`strategies/features.csv` in its data folder. With a test period kept apart ("Test on the last %"), only the bars
before it are written, so the test stays unseen.

A row a bar, with every number known at its close:

| Column | |
| --- | --- |
| `time` | The bar's open time, ms since 1970 UTC. |
| `return`, `return_1_back`, `return_2_back` | Log returns of the close: this bar's, one bar back, two bars back. |
| `rsi` | RSI 14 / 100 − 0.5. |
| `from_average` | (close − SMA 20) / ATR 14. |
| `body` | (close − open) / ATR 14. |
| `higher_next` | 1 if the next close is higher, else 0: the outcome to learn. |

The program computes the same six numbers at each close when it runs the model, so what it was trained on is what
it sees.

## 2. Train and export

The model takes a float tensor of shape `[1, 6]` (or `[n, 6]`), the columns in the order above. Its first float
output is the chance of a higher close (one number from 0 to 1) or the chances of a lower and of a higher one (two
numbers, as a classifier's probabilities).

With scikit-learn and skl2onnx:

```python
import pandas as pd
from sklearn.linear_model import LogisticRegression
from skl2onnx import to_onnx

data = pd.read_csv("features.csv")
x = data[["return", "return_1_back", "return_2_back", "rsi", "from_average", "body"]].to_numpy("float32")
y = data["higher_next"]
model = LogisticRegression().fit(x, y)
onnx = to_onnx(model, x[:1], options={id(model): {"zipmap": False}})
open("logistic.onnx", "wb").write(onnx.SerializeToString())
```

`zipmap: False` makes the probabilities a plain tensor, which the program reads. Logistic regression and random
forests give the same chances in the program as in scikit-learn. A binary `GradientBoostingClassifier` exported by
skl2onnx is refused at load.

## 3. Run it

Put the `.onnx` file into the `strategies` folder next to `features.csv`, then "Read the strategy files again".
It is listed by its file name and setting, as "Model logistic 55". A file the program cannot load is named on the
Strategy page with the reason. A model runs without the sandbox a `.wasm` strategy has (no cap on its memory or
time), so load only models you trust.
