.PHONY: test lint build install clean test-log test-integration test-integration-build

SHELL := /bin/bash
.SHELLFLAGS := -o pipefail -c

CONTAINER_ENGINE ?= podman
INTEGRATION_IMAGE = ttyforce-integration

# Use sudo for the container engine — integration tests need real root for
# losetup / loop devices, which rootless podman cannot provide even with --privileged.
SUDO := $(shell if [ "$$(id -u)" != "0" ]; then echo sudo; fi)

build:
	cargo build --release

build-dev:
	cargo build --release --features dev-tools

test: lint
	cargo test --lib --tests -- --skip integration
	$(MAKE) test-integration

lint:
	cargo check
	cargo clippy -- -D warnings

install:
	cargo install --path .

clean:
	cargo clean

# All podman calls use --network=host so apt/rustup/cargo can resolve DNS —
# the default podman bridge has no working resolver in nested/sandboxed
# environments. (--dns is incompatible with host network mode, so it is
# omitted; host networking does not alter host config so tests stay hermetic.)
test-integration-build:
	$(SUDO) $(CONTAINER_ENGINE) build --no-cache --network=host -f Containerfile.integration -t $(INTEGRATION_IMAGE) .

test-integration: test-integration-build
	$(SUDO) $(CONTAINER_ENGINE) run --rm --privileged \
		--network=host \
		--tmpfs /run \
		--tmpfs /tmp \
		-v /dev:/dev \
		$(INTEGRATION_IMAGE)

test-log:
	@mkdir -p /tmp/ttyforce-logs
	@LOGFILE="/tmp/ttyforce-logs/test-$$(date +%Y%m%d-%H%M%S).log"; \
	echo "=== Log: $$LOGFILE ==="; \
	$(MAKE) test 2>&1 | tee "$$LOGFILE"; \
	exit_code=$${PIPESTATUS[0]}; \
	echo "=== Log: $$LOGFILE ==="; \
	exit $$exit_code
