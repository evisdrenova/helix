.PHONY: bench-summary

bench-summary:
	set -e; \
	cargo bench && \
	python3 ./scripts/benchmark_summary.py && \
	ls -t benches/archive/*.md | head -n 1

