# Configuration
Specifying the location of the configuration file is required when starting `xextra`, a default configuration file can be printed by using `xextra config`. The rest of the document is a description of the file format. The configuration format is designed to reduce complexity in implementation, so is a pretty simple but strict format.

Strings will be in ASCII with `\hh` for 8-bit and control characters `like this\0a`.

## Reading
Files are interpreted as arbitrary byte-sequences (not UTF-8). The file is parsed as a series of lines, separated by `\0a` character (`\0d\0a` escapes are **not supported**) for each line, the following steps are performed
- skip any number of [Space](#space)s
- If the line is empty, the line is skipped
- If the next character is `#`, the line is skipped (comment)
- A valid Property name is taken, if no property matches, an error is emitted
- A `:` is taken.
- If the line is not empty, A [Space](#space) is taken.
- The rest of the line is used as the value of the property, and parsed according to the [Type](#types)

Valid properties are not documented here, the example configuration contains a listing of all of them.

## Space
Any character with a value less than or equal to 32

## Types
Currently available types:
- Text: Replace `\5cn` with `\0a`, `\5c0` with `\00`, and replace `\5c\xx` with `\xx`. The resulting byte string is the value
- TextList: `,`-separated list of Text values, `\5c,` does not count as a separator, trailing `,` is ignored
- Integer: Trim [Space](#space) from the start and end of the string, if the string starts with `0x` or `x`, parse as a base-16 integer, otherwise as a base-10 integer. The integer is unsigned and 32-bits large, an overflow or underflow will result in a parse error.
- Section, set the current Section. Read the value an Integer, if it is not 0 then the section is enabled.
