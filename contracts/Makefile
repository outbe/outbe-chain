# Aggregate Makefile for the contracts monorepo.
# Fans out to each sub-project. Forge projects use forge/soldeer; intex is a Hardhat hybrid.

# Self-contained Forge sub-projects (soldeer deps, forge toolchain).
FORGE_PROJECTS := intent oft vault smart-account precompiles
# Sub-projects that export ABIs via `make export-abi` (oft excluded; intex uses yarn).
ABI_PROJECTS := intent vault smart-account precompiles

.PHONY: all
all: help

## Aggregate (all sub-projects)
.PHONY: install
install: ## Restore deps everywhere (soldeer for Forge projects, yarn for intex)
	@set -e; for p in $(FORGE_PROJECTS); do echo "==> $$p (soldeer)"; ( cd $$p && forge soldeer install ); done
	@echo "==> intex (yarn)"; ( cd intex && yarn install )

.PHONY: build
build: ## Build every sub-project
	@set -e; for p in $(FORGE_PROJECTS); do echo "==> build $$p"; ( cd $$p && forge build ); done
	@echo "==> build intex"; ( cd intex && yarn compile )

.PHONY: test
test: ## Test every sub-project
	@set -e; for p in $(FORGE_PROJECTS); do echo "==> test $$p"; ( cd $$p && forge test ); done
	@echo "==> test intex"; ( cd intex && yarn test )

.PHONY: export-abi
export-abi: ## Export ABIs in every sub-project that produces them
	@set -e; for p in $(ABI_PROJECTS); do echo "==> export-abi $$p"; $(MAKE) -C $$p export-abi; done
	@echo "==> export-abi intex"; ( cd intex && yarn compile && yarn cd:extract-abi )

.PHONY: fmt
fmt: ## Format every sub-project (forge fmt)
	@set -e; for p in $(FORGE_PROJECTS) intex; do echo "==> fmt $$p"; ( cd $$p && forge fmt ); done

.PHONY: lint
lint: ## Lint every Forge sub-project (forge lint); intex lints via its own `yarn lint`
	@set -e; for p in $(FORGE_PROJECTS); do echo "==> lint $$p"; ( cd $$p && forge lint ); done

.PHONY: clean
clean: ## Clean forge artifacts in every sub-project
	@set -e; for p in $(FORGE_PROJECTS) intex; do echo "==> clean $$p"; ( cd $$p && forge clean ); done

GREEN  := $(shell tput -Txterm setaf 2)
YELLOW := $(shell tput -Txterm setaf 3)
WHITE  := $(shell tput -Txterm setaf 7)
CYAN   := $(shell tput -Txterm setaf 6)
RESET  := $(shell tput -Txterm sgr0)

## Help
.PHONY: help
help: ## Show this help
	@echo ''
	@echo 'Usage:'
	@echo '  ${YELLOW}make${RESET} ${GREEN}<target>${RESET}    (runs across all sub-projects)'
	@echo ''
	@echo 'Per-project targets live in each sub-project Makefile (e.g. make -C intent deploy-escrow).'
	@echo ''
	@echo 'Targets:'
	@awk 'BEGIN {FS = ":.*?## "} { \
		if (/^[a-zA-Z_-]+:.*?##.*$$/) {printf "    ${YELLOW}%-30s${GREEN}%s${RESET}\n", $$1, $$2} \
		else if (/^## .*$$/) {printf "  ${CYAN}%s${RESET}\n", substr($$1,4)} \
		}' $(MAKEFILE_LIST)
