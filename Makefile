# make build      release binary for this Mac            -> target/release/dime
# make universal  one binary for Apple silicon + Intel    -> dist/dime-<ver>-macos.tar.gz
# make app        double-clickable Mac app with its icon  -> dist/DiMe.app, dist/DiMe-<ver>-macos.zip
# make icon       redraw the icon from tools/make-icon.py -> packaging/DiMe.icns
# make release V=0.2.0   bump version, tag, build both, publish a GitHub release
#                        NOTES=file.md attaches written release notes instead of generated ones

# rustup's toolchain has both targets; a Homebrew rust on PATH does not
CARGO   := $(shell rustup which cargo 2>/dev/null || echo cargo)
export RUSTC := $(shell rustup which rustc 2>/dev/null || echo rustc)

NAME    := dime
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGETS := aarch64-apple-darwin x86_64-apple-darwin
TARBALL := dist/$(NAME)-$(VERSION)-macos.tar.gz
# hand-written notes if you have them, GitHub's commit list if you do not
NOTEFLAG := $(if $(NOTES),--notes-file $(NOTES),--generate-notes)

APPDIR  := dist/DiMe.app
APPZIP  := dist/DiMe-$(VERSION)-macos.zip

.PHONY: build universal app icon release check-clean

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

icon:
	python3 tools/make-icon.py

# A .app is just a folder with a plist. Beyond the icon, bundling means macOS grants Full Disk
# Access to DiMe itself instead of to whatever terminal launched it.
app: universal packaging/DiMe.icns
	rm -rf $(APPDIR)
	mkdir -p $(APPDIR)/Contents/MacOS $(APPDIR)/Contents/Resources
	cp dist/$(NAME)-$(VERSION)/$(NAME) $(APPDIR)/Contents/MacOS/$(NAME)
	cp packaging/DiMe.icns $(APPDIR)/Contents/Resources/
	sed 's/@VERSION@/$(VERSION)/g' packaging/Info.plist > $(APPDIR)/Contents/Info.plist
	codesign --force --deep --sign - $(APPDIR)   # ad-hoc: no Developer ID, but macOS stops calling it damaged
	rm -f $(APPZIP) && ditto -c -k --keepParent $(APPDIR) $(APPZIP)
	@echo built $(APPDIR) and $(APPZIP)

packaging/DiMe.icns:
	python3 tools/make-icon.py

check-clean:
	@[ -n "$(V)" ] || { echo "usage: make release V=0.2.0"; exit 1; }
	@[ -z "$$(git status --porcelain)" ] || { echo "working tree not clean"; exit 1; }
	@! git rev-parse -q --verify v$(V) >/dev/null || { echo "tag v$(V) exists"; exit 1; }

release: check-clean
	sed -i '' 's/^version = ".*"/version = "$(V)"/' Cargo.toml
	$(CARGO) check --quiet
	git add Cargo.toml Cargo.lock
	git diff --cached --quiet || git commit -q -m "v$(V)"   # already at this version: nothing to commit, still tag it
	git tag v$(V)
	git push -q origin master v$(V)
	$(MAKE) app VERSION=$(V)
	gh release create v$(V) $(NOTEFLAG) --title "v$(V)" \
		dist/DiMe-$(V)-macos.zip dist/$(NAME)-$(V)-macos.tar.gz
