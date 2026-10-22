SCHEME = TeamCraft
PROJECT = TeamCraft.xcodeproj
SIM_NAME ?= iPhone 16
DESTINATION = platform=iOS Simulator,name=$(SIM_NAME)
DERIVED = build/DerivedData
# Unsigned: simulator builds need no team; the keychain access group falls back to the app's own.
UNSIGNED = CODE_SIGNING_ALLOWED=NO CODE_SIGN_IDENTITY=""

.PHONY: core generate build test clean

# Rust core as XCFramework + Swift bindings (slow the first time).
core:
	scripts/build-xcframework.sh

generate:
	xcodegen generate

build: generate
	xcodebuild -project $(PROJECT) -scheme $(SCHEME) -destination '$(DESTINATION)' \
	  -derivedDataPath $(DERIVED) $(UNSIGNED) build

test: generate
	xcodebuild -project $(PROJECT) -scheme $(SCHEME) -destination '$(DESTINATION)' \
	  -derivedDataPath $(DERIVED) $(UNSIGNED) test

clean:
	rm -rf $(DERIVED) Generated TeamCraft.xcodeproj
