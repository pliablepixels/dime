# make build      release binary for this Mac       -> target/release/dime
# make universal  one binary for Apple silicon + Intel -> dist/dime
# make release V=0.2.0   bump version, tag, build universal, publish GitHub release with the tarball

# rustup's toolchain has both targets; a Homebrew rust on PATH does not
CARGO   := $(shell rustup which cargo 2>/dev/null || echo cargo)
export RUSTC := $(shell rustup which rustc 2>/dev/null || echo rustc)

NAME    := dime
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGETS := aarch64-apple-darwin x86_64-apple-darwin
TARBALL := dist/$(NAME)-$(VERSION)-macos.tar.gz

.PHONY: build universal release check-clean

build:
	$(CARGO) build --release

universal:
	@for t in $(TARGETS); do rustup target list --installed | grep -q $$t || rustup target add $$t; done
	for t in $(TARGETS); do $(CARGO) build --release --target $$t; done
	rm -rf dist/$(NAME)-$(VERSION) && mkdir -p dist/$(NAME)-$(VERSION)
	lipo -create -output dist/$(NAME)-$(VERSION)/$(NAME) $(foreach t,$(TARGETS),target/$(t)/release/$(NAME))
	cp LICENSE README.md dist/$(NAME)-$(VERSION)/
	tar -czf $(TARBALL) -C dist $(NAME)-$(VERSION)
	@echo built $(TARBALL); lipo -info dist/$(NAME)-$(VERSION)/$(NAME)

check-clean:
	@[ -n "$(V)" ] || { echo "usage: make release V=0.2.0"; exit 1; }
	@[ -z "$$(git status --porcelain)" ] || { echo "working tree not clean"; exit 1; }
	@! git rev-parse -q --verify v$(V) >/dev/null || { echo "tag v$(V) exists"; exit 1; }

release: check-clean
	sed -i '' 's/^version = ".*"/version = "$(V)"/' Cargo.toml
	$(CARGO) check --quiet
	git add Cargo.toml Cargo.lock && git commit -q -m "v$(V)" && git tag v$(V) && git push -q origin master v$(V)
	$(MAKE) universal VERSION=$(V)
	gh release create v$(V) --generate-notes --title "v$(V)" dist/$(NAME)-$(V)-macos.tar.gz
