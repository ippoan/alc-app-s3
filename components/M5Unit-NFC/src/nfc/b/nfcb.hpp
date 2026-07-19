/*
 * SPDX-FileCopyrightText: 2025 M5Stack Technology CO LTD
 *
 * SPDX-License-Identifier: MIT
 */
/*!
  @file nfcb.hpp
  @brief NFC-B definitions
*/
#ifndef M5_UNIT_UNIFIED_NFC_NFC_B_NFCB_HPP
#define M5_UNIT_UNIFIED_NFC_NFC_B_NFCB_HPP

#include "nfc/nfc.hpp"
#include <cstdint>
#include <string>
#include <cstring>

namespace m5 {
namespace nfc {
/*!
  @namespace b
  @brief NFC-B definitions
 */
namespace b {

/*!
  @enum Type
  @brief Type of the PICC
 */
enum class Type : uint8_t {
    Unknown,       //!< Unknown type
    Unclassified,  //!< Unclassified
};

/*!
  @enum Require
  @brief Number of slots required in the request/wakeup
 */
enum class Require : uint8_t {
    Slot1,
    Slot2,
    Slot4,
    Slot8,
    Slot16,
};

constexpr uint8_t ATQB_LENGTH{11};  //!< ATQB length pupi(4) + application(4) + protocol(3)

///@name Communication speed bits
///@{
constexpr uint8_t COMMUNICATION_SAME_SPEED{0x80};
// Correct spelling
constexpr uint8_t COMMUNICATION_SPEED_106K{0X00};
constexpr uint8_t COMMUNICATION_SPEED_212K_FROM_PICC{0X10};
constexpr uint8_t COMMUNICATION_SPEED_424K_FROM_PICC{0X20};
constexpr uint8_t COMMUNICATION_SPEED_847K_FROM_PICC{0X40};
constexpr uint8_t COMMUNICATION_SPEED_212K_TO_PICC{0X01};
constexpr uint8_t COMMUNICATION_SPEED_424K_TO_PICC{0X02};
constexpr uint8_t COMMUNICATION_SPEED_847K_TO_PICC{0X04};
// @deprecated typo aliases; use COMMUNICATION_SPEED_*
constexpr uint8_t COMMUNICATION_SPPED_106K{COMMUNICATION_SPEED_106K};
constexpr uint8_t COMMUNICATION_SPPED_212K_FROM_PICC{COMMUNICATION_SPEED_212K_FROM_PICC};
constexpr uint8_t COMMUNICATION_SPPED_424K_FROM_PICC{COMMUNICATION_SPEED_424K_FROM_PICC};
constexpr uint8_t COMMUNICATION_SPPED_847K_FROM_PICC{COMMUNICATION_SPEED_847K_FROM_PICC};
constexpr uint8_t COMMUNICATION_SPPED_212K_TO_PICC{COMMUNICATION_SPEED_212K_TO_PICC};
constexpr uint8_t COMMUNICATION_SPPED_424K_TO_PICC{COMMUNICATION_SPEED_424K_TO_PICC};
constexpr uint8_t COMMUNICATION_SPPED_847K_TO_PICC{COMMUNICATION_SPEED_847K_TO_PICC};
///@}

///@name Frame option bits
///@{
const uint8_t FRAME_OPTION_NAD{0x02};
const uint8_t FRAME_OPTION_CID{0x01};
///@}

///@name ATQB protocol
///@{
/*!
  @brief Get maximum frame length from protocol bytes
  @param protocol ATQB protocol information bytes
  @return Maximum frame length in bytes
 */
uint16_t maximum_frame_length(const uint8_t protocol[3]);

/*!
  @brief Get maximum frame length code from protocol bytes
  @param protocol ATQB protocol information bytes
  @return FSCI-like frame length bits, or RFU value if protocol is nullptr
 */
inline uint8_t maximum_frame_length_bits(const uint8_t protocol[3])
{
    return protocol ? protocol[1] >> 4 : 0x0F;
}

/*!
  @brief Supports ISO/IEC 14443-4?
  @param protocol ATQB protocol information bytes
  @return True if ISO/IEC 14443-4 is supported
 */
inline bool supports_iso14443_4(const uint8_t protocol[3])
{
    return protocol ? ((protocol[1] & 0x0F) & 0x01) : false;
}

/*!
  @brief Gets the frame option bits
  @param protocol ATQB protocol information bytes
  @return Frame option bits
 */
inline uint8_t get_frame_option(const uint8_t protocol[3])
{
    return protocol ? (protocol[2] & 0x03) : 0x00;
}
/*!
  @brief Gets the FWI
  @param protocol ATQB protocol information bytes
  @return Frame Waiting Integer, or RFU value if protocol is nullptr
 */
inline uint8_t get_fwi(const uint8_t protocol[3])
{
    return protocol ? ((protocol[2] >> 4) & 0x0F) : 0x0F /*RFU*/;
}
///@}

/*!
  @struct PICC
  @brief PICC for NFC-B
 */
struct PICC {
    uint8_t uid[8]{};
    union {
        uint8_t atqb[ATQB_LENGTH]{};  //!< ATQB
        struct {
            uint8_t pupi[4];         //!< Pseudo-Unique PICC Identifier
            uint8_t application[4];  //!< Application Data
            uint8_t protocol[3];     //!< Protocol information
            //            uint8_t cid[1];
        } __attribute__((packed));
    } __attribute__((packed));
    Type type{Type::Unknown};  //!< Type
    uint8_t cid{};             //!< CID;
    uint8_t option{};

    /*!
      @brief Valid?
      @return True if the PICC supports ISO/IEC 14443-4
     */
    inline bool valid() const
    {
        return isISO14443_4();
    }

    /*!
      @brief Gets the pupi string
      @return PUPI string
     */
    std::string pupiAsString() const;
    /*!
      @brief Gets the type string
      @return PICC type string
     */
    std::string typeAsString() const;

    ///@name Type
    ///@{
    /*!
      @brief ISO14443-4?
      @return True if the PICC supports ISO/IEC 14443-4
     */
    inline bool isISO14443_4() const
    {
        return supports_iso14443_4(protocol);
    }
    ///@}

    ///@name Information
    ///@{
    /*!
      @brief Supports NAD?
      @return True if NAD is supported
     */
    inline bool supportsNAD() const
    {
        return get_frame_option(protocol) & FRAME_OPTION_NAD;
    }
    /*!
      @brief Supports CID?
      @return True if CID is supported
     */
    inline bool supportsCID() const
    {
        return get_frame_option(protocol) & FRAME_OPTION_CID;
    }
    /*!
      @brief Gets maximum frame length
      @return Maximum frame length in bytes
     */
    inline uint16_t maximumFrameLength() const
    {
        return maximum_frame_length(protocol);
    }
    /*!
      @brief Gets maximum frame length bits
      @return Maximum frame length code from the protocol bytes
     */
    inline uint8_t maximumFrameLengthBits() const
    {
        return maximum_frame_length_bits(protocol);
    }
    /*!
      @brief Gets communication speed flags
      @return Communication speed capability byte
     */
    inline uint8_t communicationSpeed() const
    {
        return protocol[0];
    }
    /*!
      @brief Gets FWI
      @return Frame Waiting Integer
     */
    inline uint8_t fwi() const
    {
        return get_fwi(protocol);
    }
    ///@}
};

/*!
  @brief Equal?
  @param a Left PICC
  @param b Right PICC
  @return True if ATQB bytes are equal
 */
inline bool operator==(const PICC& a, const PICC& b)
{
    return std::memcmp(a.atqb, b.atqb, sizeof(a.atqb)) == 0;
}
/*!
  @brief Not equal?
  @param a Left PICC
  @param b Right PICC
  @return True if ATQB bytes are not equal
 */
inline bool operator!=(const PICC& a, const PICC& b)
{
    return !(a == b);
}

/*!
  @enum Command
  @brief  ISO/IEC 14443 Type B command (Layer 3 / activation)
 */
enum class Command : uint8_t {
    // ISO/IEC 14443B
    REQ_WUPB = 0x05,  ///< Request/Wakeup Type B
    ATTRIB   = 0x1D,  ///< Attribute (activate ISO-DEP)
    HLTB     = 0x50,  ///< Halt Type B (rarely used)
    // ISO/IEC 14443-4
    DESELECT          = 0xC2,  //!< DESELECT
    DESELECT_WITH_CID = 0xCA,  //!< DESELECT with CID
};

///@name Timeout
///@{
constexpr uint32_t TIMEOUT_REQ_WUP_B{5};
constexpr uint32_t TIMEOUT_ATTRIB{50};
constexpr uint32_t TIMEOUT_HLTB{5};
constexpr uint32_t TIMEOUT_DESELECT{5};
///@}

}  // namespace b
}  // namespace nfc
}  // namespace m5
#endif
