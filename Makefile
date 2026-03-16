.PHONY: test lint build install clean test-integration test-integration-build

CONTAINER_ENGINE ?= podman
INTEGRATION_IMAGE = ttyforce-integration

# Use sudo for the container engine if not running as root and not using podman rootless
SUDO := $(shell if [ "$$(id -u)" != "0" ] && ! $(CONTAINER_ENGINE) info >/dev/null 2>&1; then echo sudo; fi)

build:
	cargo build --release

test: lint
	cargo test --lib --tests -- --skip integration

lint:
	cargo check
	cargo clippy -- -D warnings

install:
	cargo install --path .

clean:
	cargo clean

test-integration-build:
	$(SUDO) $(CONTAINER_ENGINE) build --no-cache -f Containerfile.integration -t $(INTEGRATION_IMAGE) .

test-integration: test-integration-build
	$(SUDO) $(CONTAINER_ENGINE) run --rm --privileged \
		--tmpfs /run \
		--tmpfs /tmp \
		-v /dev:/dev \
		$(INTEGRATION_IMAGE)
