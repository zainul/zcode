(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(class_declaration name: (type_identifier) @name) @def.class
(abstract_class_declaration name: (type_identifier) @name) @def.class
(interface_declaration name: (type_identifier) @name) @def.interface
(type_alias_declaration name: (type_identifier) @name) @def.type
(enum_declaration name: (identifier) @name) @def.enum
(method_definition name: (property_identifier) @name) @def.method
(internal_module name: (identifier) @name) @def.module
(lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)])) @def.function
(lexical_declaration (variable_declarator name: (identifier) @name)) @def.const
