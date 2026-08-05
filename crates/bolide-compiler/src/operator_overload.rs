//! 类运算符重载：方法名映射（Python 风格 dunder）
//!
//! 左操作数优先 `left.__op__(right)`；否则尝试右操作数反射方法
//! `right.__rop__(left)`（比较则为对称/对偶方法）。

use bolide_parser::{BinOp, UnaryOp};

/// 左操作数上的二元运算符方法名
pub fn binop_method(op: &BinOp) -> Option<&'static str> {
    match op {
        BinOp::Add => Some("__add__"),
        BinOp::Sub => Some("__sub__"),
        BinOp::Mul => Some("__mul__"),
        BinOp::Div => Some("__div__"),
        BinOp::Mod => Some("__mod__"),
        BinOp::Eq => Some("__eq__"),
        BinOp::Ne => Some("__ne__"),
        BinOp::Lt => Some("__lt__"),
        BinOp::Le => Some("__le__"),
        BinOp::Gt => Some("__gt__"),
        BinOp::Ge => Some("__ge__"),
        BinOp::BitAnd => Some("__and__"),
        BinOp::BitOr => Some("__or__"),
        BinOp::Xor => Some("__xor__"),
        BinOp::Shl => Some("__lshift__"),
        BinOp::Shr => Some("__rshift__"),
        // 短路逻辑不重载
        BinOp::And | BinOp::Or => None,
    }
}

/// 右操作数反射方法：`right.__rop__(left)`
///
/// 算术/位运算用 `__r*__`；比较用对偶（`a < b` → `b.__gt__(a)`）。
pub fn reflected_binop_method(op: &BinOp) -> Option<&'static str> {
    match op {
        BinOp::Add => Some("__radd__"),
        BinOp::Sub => Some("__rsub__"),
        BinOp::Mul => Some("__rmul__"),
        BinOp::Div => Some("__rdiv__"),
        BinOp::Mod => Some("__rmod__"),
        BinOp::BitAnd => Some("__rand__"),
        BinOp::BitOr => Some("__ror__"),
        BinOp::Xor => Some("__rxor__"),
        BinOp::Shl => Some("__rlshift__"),
        BinOp::Shr => Some("__rrshift__"),
        // 比较：在 right 上找对偶方法
        BinOp::Eq => Some("__eq__"),
        BinOp::Ne => Some("__ne__"),
        BinOp::Lt => Some("__gt__"),
        BinOp::Le => Some("__ge__"),
        BinOp::Gt => Some("__lt__"),
        BinOp::Ge => Some("__le__"),
        BinOp::And | BinOp::Or => None,
    }
}

/// 一元运算符方法名
pub fn unary_method(op: &UnaryOp) -> Option<&'static str> {
    match op {
        UnaryOp::Neg => Some("__neg__"),
        UnaryOp::Not => Some("__not__"),
    }
}
