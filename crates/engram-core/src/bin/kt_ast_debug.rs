fn main() {
    let source = r#"package org.jetbrains.exposed.dao.id

import org.jetbrains.exposed.sql.Table
import org.jetbrains.exposed.sql.transactions.*

open class EntityID<T : Any> protected constructor(val table: Table, id: T?) {
    constructor(id: T, table: Table) : this(table, id)

    protected open fun invokeOnNoValue() {}
    override fun toString() = value.toString()

    companion object Factory {
        fun create(): EntityID<Int> = EntityID(1, MyTable)
    }
}

fun topLevelFun(x: Int): String = x.toString()

object MySingleton {
    fun doSomething() {}
}

interface MyInterface : BaseInterface {
    fun abstractMethod(): Unit
}
"#;

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_kotlin_ng::LANGUAGE.into()).expect("load kotlin");
    let tree = parser.parse(source, None).unwrap();
    
    fn print_tree(node: tree_sitter::Node, source: &[u8], indent: usize) {
        let text = if node.child_count() == 0 {
            format!(" {:?}", node.utf8_text(source).unwrap_or("?"))
        } else {
            String::new()
        };
        let field = String::new();
        println!("{}{}{} [{}-{}]{}", "  ".repeat(indent), field, node.kind(), node.start_position().row+1, node.end_position().row+1, text);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            print_tree(child, source, indent + 1);
        }
    }
    
    print_tree(tree.root_node(), source.as_bytes(), 0);
}
