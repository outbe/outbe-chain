// SPDX-License-Identifier: AGPL-3.0-only
pragma solidity 0.8.30;

/**
 * @title CreateX Factory Interface Definition
 * @author pcaversaccio (https://web.archive.org/web/20230921103111/https://pcaversaccio.com/)
 * @custom:coauthor Matt Solomon (https://web.archive.org/web/20230921103335/https://mattsolomon.dev/)
 */
interface ICreateX {
    /*':o*.o+.**'.*:o.o*.o*'.o:o*.o*.**'.*:o.o*.o*'.o:o*.o+.**'.*:*/
    /*                            TYPES                           */
    /*.*o:o.'+o.*o.o:*.'**.+o.*o:'*.'**.*o.*o:o.':*oo.*o.o:*.'+o.**/

    struct Values {
        uint256 constructorAmount;
        uint256 initCallAmount;
    }

    /*':o*.o+.**'.*:o.o*.o*'.o:o*.o*.**'.*:o.o*.o*'.o:o*.o+.**'.*:*/
    /*                           EVENTS                           */
    /*.*o:o.'+o.*o.o:*.'**.+o.*o:'*.'**.*o.*o:o.':*oo.*o.o:*.'+o.**/

    event ContractCreation(address indexed newContract, bytes32 indexed salt);
    event ContractCreation(address indexed newContract);
    event Create3ProxyContractCreation(address indexed newContract, bytes32 indexed salt);

    /*':o*.o+.**'.*:o.o*.o*'.o:o*.o*.**'.*:o.o*.o*'.o:o*.o+.**'.*:*/
    /*                        CUSTOM ERRORS                       */
    /*.*o:o.'+o.*o.o:*.'**.+o.*o:'*.'**.*o.*o:o.':*oo.*o.o:*.'+o.**/

    error FailedContractCreation(address emitter);
    error FailedContractInitialisation(address emitter, bytes revertData);
    error InvalidSalt(address emitter);
    error InvalidNonceValue(address emitter);
    error FailedEtherTransfer(address emitter, bytes revertData);

    /*':o*.o+.**'.*:o.o*.o*'.o:o*.o*.**'.*:o.o*.o*'.o:o*.o+.**'.*:*/
    /*                           CREATE                           */
    /*.*o:o.'+o.*o.o:*.'**.+o.*o:'*.'**.*o.*o:o.':*oo.*o.o:*.'+o.**/

    function deployCreate(bytes memory initCode) external payable returns (address newContract);

    function deployCreateAndInit(bytes memory initCode, bytes memory data, Values memory values, address refundAddress)
        external
        payable
        returns (address newContract);

    function deployCreateAndInit(bytes memory initCode, bytes memory data, Values memory values)
        external
        payable
        returns (address newContract);

    function deployCreateClone(address implementation, bytes memory data) external payable returns (address proxy);

    function computeCreateAddress(address deployer, uint256 nonce) external view returns (address computedAddress);

    function computeCreateAddress(uint256 nonce) external view returns (address computedAddress);

    /*':o*.o+.**'.*:o.o*.o*'.o:o*.o*.**'.*:o.o*.o*'.o:o*.o+.**'.*:*/
    /*                           CREATE2                          */
    /*.*o:o.'+o.*o.o:*.'**.+o.*o:'*.'**.*o.*o:o.':*oo.*o.o:*.'+o.**/

    function deployCreate2(bytes32 salt, bytes memory initCode) external payable returns (address newContract);

    function deployCreate2(bytes memory initCode) external payable returns (address newContract);

    function deployCreate2AndInit(
        bytes32 salt,
        bytes memory initCode,
        bytes memory data,
        Values memory values,
        address refundAddress
    ) external payable returns (address newContract);

    function deployCreate2AndInit(bytes32 salt, bytes memory initCode, bytes memory data, Values memory values)
        external
        payable
        returns (address newContract);

    function deployCreate2AndInit(bytes memory initCode, bytes memory data, Values memory values, address refundAddress)
        external
        payable
        returns (address newContract);

    function deployCreate2AndInit(bytes memory initCode, bytes memory data, Values memory values)
        external
        payable
        returns (address newContract);

    function deployCreate2Clone(bytes32 salt, address implementation, bytes memory data)
        external
        payable
        returns (address proxy);

    function deployCreate2Clone(address implementation, bytes memory data) external payable returns (address proxy);

    function computeCreate2Address(bytes32 salt, bytes32 initCodeHash, address deployer)
        external
        pure
        returns (address computedAddress);

    function computeCreate2Address(bytes32 salt, bytes32 initCodeHash) external view returns (address computedAddress);

    /*':o*.o+.**'.*:o.o*.o*'.o:o*.o*.**'.*:o.o*.o*'.o:o*.o+.**'.*:*/
    /*                           CREATE3                          */
    /*.*o:o.'+o.*o.o:*.'**.+o.*o:'*.'**.*o.*o:o.':*oo.*o.o:*.'+o.**/

    function deployCreate3(bytes32 salt, bytes memory initCode) external payable returns (address newContract);

    function deployCreate3(bytes memory initCode) external payable returns (address newContract);

    function deployCreate3AndInit(
        bytes32 salt,
        bytes memory initCode,
        bytes memory data,
        Values memory values,
        address refundAddress
    ) external payable returns (address newContract);

    function deployCreate3AndInit(bytes32 salt, bytes memory initCode, bytes memory data, Values memory values)
        external
        payable
        returns (address newContract);

    function deployCreate3AndInit(bytes memory initCode, bytes memory data, Values memory values, address refundAddress)
        external
        payable
        returns (address newContract);

    function deployCreate3AndInit(bytes memory initCode, bytes memory data, Values memory values)
        external
        payable
        returns (address newContract);

    function computeCreate3Address(bytes32 salt, address deployer) external pure returns (address computedAddress);

    function computeCreate3Address(bytes32 salt) external view returns (address computedAddress);
}
