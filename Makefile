.PHONY: help test lint build build-dev install clean test-log test-integration test-integration-build

SHELL := /bin/bash
.SHELLFLAGS := -o pipefail -c

# Keep `make` (no target) building, as before — `make help` lists everything.
.DEFAULT_GOAL := build

CONTAINER_ENGINE ?= podman
INTEGRATION_IMAGE = ttyforce-integration

# Use sudo for the container engine — integration tests need real root for
# losetup / loop devices, which rootless podman cannot provide even with --privileged.
SUDO := $(shell if [ "$$(id -u)" != "0" ]; then echo sudo; fi)

help: ## Show this help
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-24s\033[0m %s\n", $$1, $$2}'

build: ## Build the release binary
	cargo build --release

build-dev: ## Build the release binary with dev-tools features
	cargo build --release --features dev-tools

test: lint ## Run lint, unit tests, then integration tests
	cargo test --lib --tests -- --skip integration
	$(MAKE) test-integration

lint: ## Run cargo check and clippy (warnings as errors)
	cargo check
	cargo clippy -- -D warnings

install: ## Install the binary via cargo install
	cargo install --path .

clean: ## Remove build artifacts (cargo clean)
	cargo clean

# All podman calls use --network=host so apt/rustup/cargo can resolve DNS —
# the default podman bridge has no working resolver in nested/sandboxed
# environments. (--dns is incompatible with host network mode, so it is
# omitted; host networking does not alter host config so tests stay hermetic.)
test-integration-build: ## Build the integration test container image
	$(SUDO) $(CONTAINER_ENGINE) build --no-cache --network=host -f Containerfile.integration -t $(INTEGRATION_IMAGE) .

test-integration: test-integration-build ## Run integration tests in the container
	$(SUDO) $(CONTAINER_ENGINE) run --rm --privileged \
		--network=host \
		--tmpfs /run \
		--tmpfs /tmp \
		-v /dev:/dev \
		$(INTEGRATION_IMAGE)

test-log: ## Run `make test`, teeing output to a timestamped log file

	@mkdir -p /tmp/ttyforce-logs
	@LOGFILE="/tmp/ttyforce-logs/test-$$(date +%Y%m%d-%H%M%S).log"; \
	echo "=== Log: $$LOGFILE ==="; \
	$(MAKE) test 2>&1 | tee "$$LOGFILE"; \
	exit_code=$${PIPESTATUS[0]}; \
	echo "=== Log: $$LOGFILE ==="; \
	exit $$exit_code
