# Security

## Reporting a vulnerability

Report it privately: on the repository's Security tab, "Report a vulnerability". Please do not open a public issue
for it. Say what is affected, how to reproduce it and which version you used.

## What is in scope

- The program itself, as released here.
- Loading strategies (`.wasm`) and models (`.onnx`) from the `strategies` folder.
- Parsing what comes in: answers and archive files of the exchanges and data sources, CSV files imported, the
  program's own files in its data folder (sessions, the watchlist, the cache).

## What the program protects against

A `.wasm` strategy runs in a sandbox: it can reach no files and no network, and its memory and work are capped.
A strategy that gets past that is a vulnerability.

An `.onnx` model has no such sandbox: the program runs it with no cap on its memory or time, so load only models
you trust. A model that crashes or freezes the program is a bug, not a vulnerability; one that runs code it should
not is a vulnerability.

The program uses only the public market data of the exchanges: it holds no keys and places no orders.
