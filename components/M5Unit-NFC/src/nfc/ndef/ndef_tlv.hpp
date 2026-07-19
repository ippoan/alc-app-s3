/*
 * SPDX-FileCopyrightText: 2025 M5Stack Technology CO LTD
 *
 * SPDX-License-Identifier: MIT
 */
/*!
  @file ndef_tlv.hpp
  @brief NDEF TLV
*/
#ifndef M5_UNIT_UNIFIED_NFC_NDEF_NDEF_TLV_HPP
#define M5_UNIT_UNIFIED_NFC_NDEF_NDEF_TLV_HPP

#include "ndef.hpp"
#include "ndef_record.hpp"
#include <vector>

namespace m5 {
namespace nfc {
namespace ndef {

class Record;

/*!
  @class TLV
  @brief NDEF TLV container
 */
class TLV {
public:
    using container_type = std::vector<Record>;

    //!@brief Terminator instance
    static const TLV Terminator;

    //! @brief Default ctor (Tag::Null)
    TLV() : TLV(Tag::Null)
    {
    }
    /*!
      @brief Construct with the given Tag
      @param t Tag
     */
    explicit TLV(const Tag t) : _tag{t}
    {
    }
    //! @brief Destructor
    ~TLV()
    {
    }

    /*!
      @brief Tag
      @return TLV tag
     */
    inline Tag tag() const
    {
        return _tag;
    }
    /*!
      @brief Is terminator
      @return True if this TLV is a Terminator TLV
     */
    inline bool isTerminatorTLV() const
    {
        return _tag == Tag::Terminator;
    }
    /*!
      @brief Is Message?
      @return True if this TLV is a Message TLV
     */
    inline bool isMessageTLV() const
    {
        return _tag == Tag::Message;
    }
    /*!
      @brief Is Null TLV?
      @return True if this TLV is a Null TLV
     */
    inline bool isNullTLV() const
    {
        return _tag == Tag::Null;
    }
    /*!
      @brief Get the records
      @return NDEF records
      @pre Tag must be Message
    */
    inline const container_type& records() const
    {
        return _records;
    }
    /*!
      @brief Get the payload
      @return TLV payload bytes
      @pre Tag must NOT be Message
    */
    inline const std::vector<uint8_t>& payload() const
    {
        return _payload;
    }
    /*!
      @brief Get the payload
      @return Mutable TLV payload bytes
      @pre Tag must NOT be Message
    */
    inline std::vector<uint8_t>& payload()
    {
        return _payload;
    }

    /*!
      @brief Size required for encoding
      @return Required encoded size in bytes
     */
    uint32_t required() const;

    /*!
      @brief Push back the record
      @param r Record
      @return True if successful
      @note A copy of the Record is inserted at the end
     */
    bool push_back(const Record& r);

    /*!
      @brief Removes the last record
      @note Does nothing if there is no record
     */
    void pop_back();

    /*!
      @brief Encode
      @param[out] buf Buffer
      @param blen Buffer size
      @retval > 0 Encoded length
      @retval == 0 Error
     */
    uint32_t encode(uint8_t* buf, const uint32_t blen) const;
    /*!
      @brief Decode
      @param buf Pointer of the TLV
      @param len Buffer length
      @retval > 0 Decoded length
      @retval == 0 Error
     */
    uint32_t decode(const uint8_t* buf, const uint32_t len);

    /*!
      @brief Clear internal buffers
      @warning Keep the tag
    */
    void clear();

    //! @brief Dump internal state for debugging
    void dump();

private:
    Tag _tag{};
    container_type _records{};
    std::vector<uint8_t> _payload{};
};
}  // namespace ndef
}  // namespace nfc
}  // namespace m5
#endif
