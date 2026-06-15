.PHONY: all
all: help

## Solidity
.PHONY: install
install: ## Install dependencies using soldeer
	forge soldeer install

.PHONY: build
build: ## Build Solidity contracts using forge
	forge build

.PHONY: test
test: ## Test Solidity contracts using forge
	forge test

.PHONY: fmt
fmt: ## Format Solidity code
	forge fmt

.PHONY: lint
lint: ## Lint Solidity code
	forge lint

.PHONY: clean
clean: ## Clean up forge artifacts
	forge clean

.PHONY: clean-all
clean-all: clean  ## Clean all build artifacts, and caches
	forge soldeer clean
	rm -rf ./out
	rm -rf ./build
	rm -rf ./cache

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
	@echo '  ${YELLOW}make${RESET} ${GREEN}<target>${RESET}'
	@echo ''
	@echo 'Targets:'
	@awk 'BEGIN {FS = ":.*?## "} { \
		if (/^[a-zA-Z_-]+:.*?##.*$$/) {printf "    ${YELLOW}%-30s${GREEN}%s${RESET}\n", $$1, $$2} \
		else if (/^## .*$$/) {printf "  ${CYAN}%s${RESET}\n", substr($$1,4)} \
		}' $(MAKEFILE_LIST)
