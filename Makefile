.PHONY: prepare lint check test update install

prepare: lint check

lint:
	uv run ruff check --select I --fix .
	uv run ruff format .

check:
	uv run ruff format --check .
	uv run ruff check .
	uv run ty check

test:
	uv run pytest -s --cov=cctrail tests/

update:
	uv sync --upgrade --all-groups

install:
	uv sync
