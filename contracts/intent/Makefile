# Include environment-specific .env file (loads NETWORK and other vars for deploy targets).
# Override ENV_FILE to use a different environment (e.g., make deploy-escrow ENV_FILE=.bsc-testnet.env)
ENV_FILE ?= .env
-include $(ENV_FILE)
export

.PHONY: all
all: help

## Solidity
.PHONY: build
build: ## Build Solidity contracts using forge
	forge build

.PHONY: install
install: ## Restore soldeer dependencies
	forge soldeer install

.PHONY: test
test: ## Test Solidity contracts using forge
	forge test

.PHONY: fmt
fmt: ## Format Solidity code
	forge fmt

.PHONY: lint
lint: ## Lint Solidity code
	forge lint

.PHONY: export-abi
export-abi: build ## Export contracts ABI to ./abi-export
	@echo 'Exporting ABI for intent contracts'
	mkdir -p ./abi-export
	rm -rf ./abi-export/*.json
	for contract in \
		LayerZeroRouter \
		ERC20 \
		SolverEscrow \
		Auction ; \
	do \
		cat ./out/$${contract}.sol/$${contract}.json | jq -r '.abi' > ./abi-export/$${contract}.json; \
	done

.PHONY: clean
clean: ## Clean up forge artifacts
	forge clean

.PHONY: clean-all
clean-all: clean ## Clean all build artifacts and caches
	rm -rf ./out ./cache

## Deploy (require NETWORK + keys in $(ENV_FILE))
.PHONY: deploy-createx
deploy-createx: ## Deploy CreateX deterministic factory
	forge script script/0_DeployCreateX.s.sol:DeployCreateXDeterministic --rpc-url $(NETWORK) --broadcast --verify --slow -vvv

.PHONY: deploy-escrow
deploy-escrow: ## Deploy SolverEscrow
	forge script script/1_DeploySolverEscrow.s.sol:DeploySolverEscrow --rpc-url $(NETWORK) --broadcast --verify --slow --legacy -vvv --skip-simulation

.PHONY: deploy-auction
deploy-auction: ## Deploy Auction
	forge script script/2_DeployAuction.s.sol:DeployAuction --rpc-url $(NETWORK) --broadcast --verify --slow --legacy -vvv

.PHONY: deploy-lz-router
deploy-lz-router: ## Deploy LayerZeroRouter
	forge script script/3_DeployLayerZeroRouter.s.sol:DeployLayerZeroRouter --rpc-url $(NETWORK) --broadcast --verify --slow --legacy -vvv

.PHONY: configure-all
configure-all: ## Run ConfigureAll
	forge script script/4_ConfigureAll.s.sol:ConfigureAll --rpc-url $(NETWORK) --broadcast --slow --legacy -vvv --skip-simulation

.PHONY: configure-lz-peers
configure-lz-peers: ## Set up LayerZero peers
	forge script script/SetupLZPeers.s.sol:SetupLZPeers --rpc-url $(NETWORK) --broadcast --slow --legacy -vvv --skip-simulation

.PHONY: deploy-all
deploy-all: ## Deploy everything
	forge script script/DeployAll.s.sol:DeployAll --rpc-url $(NETWORK) --broadcast --slow --legacy -vvv --skip-simulation

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
