# levelize — build, man page, install. `make help` lists the targets.

PREFIX ?= /usr/local
BINDIR  = $(DESTDIR)$(PREFIX)/bin
MANDIR  = $(DESTDIR)$(PREFIX)/share/man/man1
CARGO  ?= cargo

.PHONY: all build man test lint install uninstall deb rpm clean help

all: build man ## Build the release binary and the man page

build: ## Build the release binary (target/release/levelize)
	$(CARGO) build --release

man: doc/levelize.1 ## Generate the man page into doc/levelize.1

doc/levelize.1: src/cli.rs Cargo.toml
	mkdir -p doc
	$(CARGO) run --quiet --release --features mangen --bin levelize-man > $@

test: ## Run the test suite (all features)
	$(CARGO) test --all-features

lint: ## Check formatting and clippy lints
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets --all-features -- -D warnings

install: build man ## Install binary and man page under PREFIX (default /usr/local)
	install -Dm755 target/release/levelize $(BINDIR)/levelize
	install -Dm644 doc/levelize.1 $(MANDIR)/levelize.1

uninstall: ## Remove the installed binary and man page
	rm -f $(BINDIR)/levelize $(MANDIR)/levelize.1

deb: build man ## Build a .deb into target/debian (needs cargo-deb)
	$(CARGO) deb --no-build --no-strip

rpm: build man ## Build an .rpm into target/generate-rpm (needs cargo-generate-rpm)
	$(CARGO) generate-rpm

clean: ## Remove build artifacts and the generated man page
	$(CARGO) clean
	rm -f doc/levelize.1

help: ## Show this help
	@grep -E '^[a-zA-Z0-9_-]+:.*## ' $(MAKEFILE_LIST) \
		| awk -F':.*## ' '{printf "  %-10s %s\n", $$1, $$2}'
