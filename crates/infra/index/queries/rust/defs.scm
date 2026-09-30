; Definitions (CE-DQ15). @name is the identifier; the other capture is the node.
(function_item name: (identifier) @name) @def.function
(function_signature_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.struct
(union_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(trait_item name: (type_identifier) @name) @def.trait
(type_item name: (type_identifier) @name) @def.type
(const_item name: (identifier) @name) @def.const
(static_item name: (identifier) @name) @def.const
(mod_item name: (identifier) @name) @def.module
(macro_definition name: (identifier) @name) @def.function
(impl_item type: (_) @name) @def.impl
