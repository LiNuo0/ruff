//! Smart builders for union and intersection types.
//!
//! Invariants we maintain here:
//!   * No single-element union types (should just be the contained type instead.)
//!   * No single-positive-element intersection types. Single-negative-element are OK, we don't
//!     have a standalone negation type so there's no other representation for this.
//!   * The same type should never appear more than once in a union or intersection. (This should
//!     be expanded to cover subtyping -- see below -- but for now we only implement it for type
//!     identity.)
//!   * Disjunctive normal form (DNF): the tree of unions and intersections can never be deeper
//!     than a union-of-intersections. Unions cannot contain other unions (the inner union just
//!     flattens into the outer one), intersections cannot contain other intersections (also
//!     flattens), and intersections cannot contain unions (the intersection distributes over the
//!     union, inverting it into a union-of-intersections).
//!
//! The implication of these invariants is that a [`UnionBuilder`] does not necessarily build a
//! [`Type::Union`]. For example, if only one type is added to the [`UnionBuilder`], `build()` will
//! just return that type directly. The same is true for [`IntersectionBuilder`]; for example, if a
//! union type is added to the intersection, it will distribute and [`IntersectionBuilder::build`]
//! may end up returning a [`Type::Union`] of intersections.
//!
//! In the future we should have these additional invariants, but they aren't implemented yet:
//!   * No type in a union can be a subtype of any other type in the union (just eliminate the
//!     subtype from the union).
//!   * No type in an intersection can be a supertype of any other type in the intersection (just
//!     eliminate the supertype from the intersection).
//!   * An intersection containing two non-overlapping types should simplify to [`Type::Never`].

use crate::types::{InstanceType, IntersectionType, KnownClass, Type, UnionType};
use crate::{Db, FxOrderSet};
use smallvec::SmallVec;

use super::Truthiness;

pub(crate) struct UnionBuilder<'db> {
    elements: Vec<Type<'db>>,
    db: &'db dyn Db,
}

impl<'db> UnionBuilder<'db> {
    pub(crate) fn new(db: &'db dyn Db) -> Self {
        Self {
            db,
            elements: vec![],
        }
    }

    /// Adds a type to this union.
    pub(crate) fn add(mut self, ty: Type<'db>) -> Self {
        match ty {
            Type::Union(union) => {
                let new_elements = union.elements(self.db);
                self.elements.reserve(new_elements.len());
                for element in new_elements {
                    self = self.add(*element);
                }
            }
            Type::Never => {}
            _ => {
                let bool_pair = if let Type::BooleanLiteral(b) = ty {
                    Some(Type::BooleanLiteral(!b))
                } else {
                    None
                };

                let mut to_add = ty;
                let mut to_remove = SmallVec::<[usize; 2]>::new();
                for (index, element) in self.elements.iter().enumerate() {
                    if Some(*element) == bool_pair {
                        to_add = KnownClass::Bool.to_instance(self.db);
                        to_remove.push(index);
                        // The type we are adding is a BooleanLiteral, which doesn't have any
                        // subtypes. And we just found that the union already contained our
                        // mirror-image BooleanLiteral, so it can't also contain bool or any
                        // supertype of bool. Therefore, we are done.
                        break;
                    }

                    if ty.is_same_gradual_form(*element) || ty.is_subtype_of(self.db, *element) {
                        return self;
                    } else if element.is_subtype_of(self.db, ty) {
                        to_remove.push(index);
                    }
                }
                match to_remove[..] {
                    [] => self.elements.push(to_add),
                    [index] => self.elements[index] = to_add,
                    _ => {
                        let mut current_index = 0;
                        let mut to_remove = to_remove.into_iter();
                        let mut next_to_remove_index = to_remove.next();
                        self.elements.retain(|_| {
                            let retain = if Some(current_index) == next_to_remove_index {
                                next_to_remove_index = to_remove.next();
                                false
                            } else {
                                true
                            };
                            current_index += 1;
                            retain
                        });
                        self.elements.push(to_add);
                    }
                }
            }
        }
        self
    }

    pub(crate) fn build(self) -> Type<'db> {
        match self.elements.len() {
            0 => Type::Never,
            1 => self.elements[0],
            _ => Type::Union(UnionType::new(self.db, self.elements.into_boxed_slice())),
        }
    }
}

#[derive(Clone)]
pub(crate) struct IntersectionBuilder<'db> {
    // Really this builds a union-of-intersections, because we always keep our set-theoretic types
    // in disjunctive normal form (DNF), a union of intersections. In the simplest case there's
    // just a single intersection in this vector, and we are building a single intersection type,
    // but if a union is added to the intersection, we'll distribute ourselves over that union and
    // create a union of intersections.
    intersections: Vec<InnerIntersectionBuilder<'db>>,
    db: &'db dyn Db,
}

impl<'db> IntersectionBuilder<'db> {
    pub(crate) fn new(db: &'db dyn Db) -> Self {
        Self {
            db,
            intersections: vec![InnerIntersectionBuilder::default()],
        }
    }

    fn empty(db: &'db dyn Db) -> Self {
        Self {
            db,
            intersections: vec![],
        }
    }

    pub(crate) fn add_positive(mut self, ty: Type<'db>) -> Self {
        if let Type::Union(union) = ty {
            // Distribute ourself over this union: for each union element, clone ourself and
            // intersect with that union element, then create a new union-of-intersections with all
            // of those sub-intersections in it. E.g. if `self` is a simple intersection `T1 & T2`
            // and we add `T3 | T4` to the intersection, we don't get `T1 & T2 & (T3 | T4)` (that's
            // not in DNF), we distribute the union and get `(T1 & T3) | (T2 & T3) | (T1 & T4) |
            // (T2 & T4)`. If `self` is already a union-of-intersections `(T1 & T2) | (T3 & T4)`
            // and we add `T5 | T6` to it, that flattens all the way out to `(T1 & T2 & T5) | (T1 &
            // T2 & T6) | (T3 & T4 & T5) ...` -- you get the idea.
            union
                .elements(self.db)
                .iter()
                .map(|elem| self.clone().add_positive(*elem))
                .fold(IntersectionBuilder::empty(self.db), |mut builder, sub| {
                    builder.intersections.extend(sub.intersections);
                    builder
                })
        } else {
            // If we are already a union-of-intersections, distribute the new intersected element
            // across all of those intersections.
            for inner in &mut self.intersections {
                inner.add_positive(self.db, ty);
            }
            self
        }
    }

    pub(crate) fn add_negative(mut self, ty: Type<'db>) -> Self {
        // See comments above in `add_positive`; this is just the negated version.
        if let Type::Union(union) = ty {
            for elem in union.elements(self.db) {
                self = self.add_negative(*elem);
            }
            self
        } else if let Type::Intersection(intersection) = ty {
            // (A | B) & ~(C & ~D)
            // -> (A | B) & (~C | D)
            // -> ((A | B) & ~C) | ((A | B) & D)
            // i.e. if we have an intersection of positive constraints C
            // and negative constraints D, then our new intersection
            // is (existing & ~C) | (existing & D)

            let positive_side = intersection
                .positive(self.db)
                .iter()
                // we negate all the positive constraints while distributing
                .map(|elem| self.clone().add_negative(*elem));

            let negative_side = intersection
                .negative(self.db)
                .iter()
                // all negative constraints end up becoming positive constraints
                .map(|elem| self.clone().add_positive(*elem));

            positive_side.chain(negative_side).fold(
                IntersectionBuilder::empty(self.db),
                |mut builder, sub| {
                    builder.intersections.extend(sub.intersections);
                    builder
                },
            )
        } else {
            for inner in &mut self.intersections {
                inner.add_negative(self.db, ty);
            }
            self
        }
    }

    pub(crate) fn build(mut self) -> Type<'db> {
        // Avoid allocating the UnionBuilder unnecessarily if we have just one intersection:
        if self.intersections.len() == 1 {
            self.intersections.pop().unwrap().build(self.db)
        } else {
            UnionType::from_elements(
                self.db,
                self.intersections
                    .into_iter()
                    .map(|inner| inner.build(self.db)),
            )
        }
    }
}

#[derive(Debug, Clone, Default)]
struct InnerIntersectionBuilder<'db> {
    positive: FxOrderSet<Type<'db>>,
    negative: FxOrderSet<Type<'db>>,
}

impl<'db> InnerIntersectionBuilder<'db> {
    /// Adds a positive type to this intersection.
    fn add_positive(&mut self, db: &'db dyn Db, new_positive: Type<'db>) {
        if let Type::Intersection(other) = new_positive {
            for pos in other.positive(db) {
                self.add_positive(db, *pos);
            }
            for neg in other.negative(db) {
                self.add_negative(db, *neg);
            }
        } else {
            // ~Literal[True] & bool = Literal[False]
            // ~AlwaysTruthy & bool = Literal[False]
            if let Type::Instance(InstanceType { class }) = new_positive {
                if class.is_known(db, KnownClass::Bool) {
                    if let Some(new_type) = self
                        .negative
                        .iter()
                        .find(|element| {
                            element.is_boolean_literal()
                                | matches!(element, Type::AlwaysFalsy | Type::AlwaysTruthy)
                        })
                        .map(|element| {
                            Type::BooleanLiteral(element.bool(db) != Truthiness::AlwaysTrue)
                        })
                    {
                        *self = Self::default();
                        self.positive.insert(new_type);
                        return;
                    }
                }
            }

            let mut to_remove = SmallVec::<[usize; 1]>::new();
            for (index, existing_positive) in self.positive.iter().enumerate() {
                // S & T = S    if S <: T
                if existing_positive.is_subtype_of(db, new_positive)
                    || existing_positive.is_same_gradual_form(new_positive)
                {
                    return;
                }
                // same rule, reverse order
                if new_positive.is_subtype_of(db, *existing_positive) {
                    to_remove.push(index);
                }
                // A & B = Never    if A and B are disjoint
                if new_positive.is_disjoint_from(db, *existing_positive) {
                    *self = Self::default();
                    self.positive.insert(Type::Never);
                    return;
                }
            }
            for index in to_remove.iter().rev() {
                self.positive.swap_remove_index(*index);
            }

            let mut to_remove = SmallVec::<[usize; 1]>::new();
            for (index, existing_negative) in self.negative.iter().enumerate() {
                // S & ~T = Never    if S <: T
                if new_positive.is_subtype_of(db, *existing_negative) {
                    *self = Self::default();
                    self.positive.insert(Type::Never);
                    return;
                }
                // A & ~B = A    if A and B are disjoint
                if existing_negative.is_disjoint_from(db, new_positive) {
                    to_remove.push(index);
                }
            }
            for index in to_remove.iter().rev() {
                self.negative.swap_remove_index(*index);
            }

            self.positive.insert(new_positive);
        }
    }

    /// Adds a negative type to this intersection.
    fn add_negative(&mut self, db: &'db dyn Db, new_negative: Type<'db>) {
        match new_negative {
            Type::Intersection(inter) => {
                for pos in inter.positive(db) {
                    self.add_negative(db, *pos);
                }
                for neg in inter.negative(db) {
                    self.add_positive(db, *neg);
                }
            }
            ty @ (Type::Any | Type::Unknown | Type::Todo(_)) => {
                // Adding any of these types to the negative side of an intersection
                // is equivalent to adding it to the positive side. We do this to
                // simplify the representation.
                self.add_positive(db, ty);
            }
            // bool & ~Literal[True] = Literal[False]
            // bool & ~AlwaysTruthy = Literal[False]
            Type::BooleanLiteral(_) | Type::AlwaysFalsy | Type::AlwaysTruthy
                if self.positive.contains(&KnownClass::Bool.to_instance(db)) =>
            {
                *self = Self::default();
                self.positive.insert(Type::BooleanLiteral(
                    new_negative.bool(db) != Truthiness::AlwaysTrue,
                ));
            }
            _ => {
                let mut to_remove = SmallVec::<[usize; 1]>::new();
                for (index, existing_negative) in self.negative.iter().enumerate() {
                    // ~S & ~T = ~T    if S <: T
                    if existing_negative.is_subtype_of(db, new_negative) {
                        to_remove.push(index);
                    }
                    // same rule, reverse order
                    if new_negative.is_subtype_of(db, *existing_negative) {
                        return;
                    }
                }
                for index in to_remove.iter().rev() {
                    self.negative.swap_remove_index(*index);
                }

                for existing_positive in &self.positive {
                    // S & ~T = Never    if S <: T
                    if existing_positive.is_subtype_of(db, new_negative) {
                        *self = Self::default();
                        self.positive.insert(Type::Never);
                        return;
                    }
                    // A & ~B = A    if A and B are disjoint
                    if existing_positive.is_disjoint_from(db, new_negative) {
                        return;
                    }
                }

                self.negative.insert(new_negative);
            }
        }
    }

    fn build(mut self, db: &'db dyn Db) -> Type<'db> {
        match (self.positive.len(), self.negative.len()) {
            (0, 0) => KnownClass::Object.to_instance(db),
            (1, 0) => self.positive[0],
            _ => {
                self.positive.shrink_to_fit();
                self.negative.shrink_to_fit();
                Type::Intersection(IntersectionType::new(db, self.positive, self.negative))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IntersectionBuilder, IntersectionType, Type, UnionType};

    use crate::db::tests::{setup_db, TestDb};
    use crate::types::{global_symbol, todo_type, KnownClass, Truthiness, UnionBuilder};

    use ruff_db::files::system_path_to_file;
    use ruff_db::system::DbWithTestSystem;
    use test_case::test_case;

    #[test]
    fn build_union_no_elements() {
        let db = setup_db();
        let ty = UnionBuilder::new(&db).build();
        assert_eq!(ty, Type::Never);
    }

    #[test]
    fn build_union_single_element() {
        let db = setup_db();
        let t0 = Type::IntLiteral(0);
        let ty = UnionType::from_elements(&db, [t0]);
        assert_eq!(ty, t0);
    }

    #[test]
    fn build_union_two_elements() {
        let db = setup_db();
        let t0 = Type::IntLiteral(0);
        let t1 = Type::IntLiteral(1);
        let union = UnionType::from_elements(&db, [t0, t1]).expect_union();

        assert_eq!(union.elements(&db), &[t0, t1]);
    }

    impl<'db> IntersectionType<'db> {
        fn pos_vec(self, db: &'db TestDb) -> Vec<Type<'db>> {
            self.positive(db).into_iter().copied().collect()
        }

        fn neg_vec(self, db: &'db TestDb) -> Vec<Type<'db>> {
            self.negative(db).into_iter().copied().collect()
        }
    }

    #[test]
    fn build_intersection() {
        let db = setup_db();
        let t0 = Type::IntLiteral(0);
        let ta = Type::Any;
        let intersection = IntersectionBuilder::new(&db)
            .add_positive(ta)
            .add_negative(t0)
            .build()
            .expect_intersection();

        assert_eq!(intersection.pos_vec(&db), &[ta]);
        assert_eq!(intersection.neg_vec(&db), &[t0]);
    }

    #[test]
    fn build_intersection_empty_intersection_equals_object() {
        let db = setup_db();

        let ty = IntersectionBuilder::new(&db).build();

        assert_eq!(ty, KnownClass::Object.to_instance(&db));
    }

    #[test]
    fn build_intersection_flatten_positive() {
        let db = setup_db();
        let ta = Type::Any;
        let t1 = Type::IntLiteral(1);
        let t2 = Type::IntLiteral(2);
        let i0 = IntersectionBuilder::new(&db)
            .add_positive(ta)
            .add_negative(t1)
            .build();
        let intersection = IntersectionBuilder::new(&db)
            .add_positive(t2)
            .add_positive(i0)
            .build()
            .expect_intersection();

        assert_eq!(intersection.pos_vec(&db), &[t2, ta]);
        assert_eq!(intersection.neg_vec(&db), &[]);
    }

    #[test]
    fn build_intersection_flatten_negative() {
        let db = setup_db();
        let ta = Type::Any;
        let t1 = Type::IntLiteral(1);
        let t2 = KnownClass::Int.to_instance(&db);
        // i0 = Any & ~Literal[1]
        let i0 = IntersectionBuilder::new(&db)
            .add_positive(ta)
            .add_negative(t1)
            .build();
        // ta_not_i0 = int & ~(Any & ~Literal[1])
        // -> int & (~Any  | Literal[1])
        // (~Any is equivalent to Any)
        // -> (int & Any) | (int & Literal[1])
        // -> (int & Any) | Literal[1]
        let ta_not_i0 = IntersectionBuilder::new(&db)
            .add_positive(t2)
            .add_negative(i0)
            .build();

        assert_eq!(ta_not_i0.display(&db).to_string(), "int & Any | Literal[1]");
    }

    #[test]
    fn build_intersection_simplify_negative_any() {
        let db = setup_db();

        let ty = IntersectionBuilder::new(&db)
            .add_negative(Type::Any)
            .build();
        assert_eq!(ty, Type::Any);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(Type::Never)
            .add_negative(Type::Any)
            .build();
        assert_eq!(ty, Type::Never);
    }

    #[test]
    fn build_intersection_simplify_multiple_unknown() {
        let db = setup_db();

        let ty = IntersectionBuilder::new(&db)
            .add_positive(Type::Unknown)
            .add_positive(Type::Unknown)
            .build();
        assert_eq!(ty, Type::Unknown);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(Type::Unknown)
            .add_negative(Type::Unknown)
            .build();
        assert_eq!(ty, Type::Unknown);

        let ty = IntersectionBuilder::new(&db)
            .add_negative(Type::Unknown)
            .add_negative(Type::Unknown)
            .build();
        assert_eq!(ty, Type::Unknown);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(Type::Unknown)
            .add_positive(Type::IntLiteral(0))
            .add_negative(Type::Unknown)
            .build();
        assert_eq!(
            ty,
            IntersectionBuilder::new(&db)
                .add_positive(Type::Unknown)
                .add_positive(Type::IntLiteral(0))
                .build()
        );
    }

    #[test]
    fn intersection_distributes_over_union() {
        let db = setup_db();
        let t0 = Type::IntLiteral(0);
        let t1 = Type::IntLiteral(1);
        let ta = Type::Any;
        let u0 = UnionType::from_elements(&db, [t0, t1]);

        let union = IntersectionBuilder::new(&db)
            .add_positive(ta)
            .add_positive(u0)
            .build()
            .expect_union();
        let [Type::Intersection(i0), Type::Intersection(i1)] = union.elements(&db)[..] else {
            panic!("expected a union of two intersections");
        };
        assert_eq!(i0.pos_vec(&db), &[ta, t0]);
        assert_eq!(i1.pos_vec(&db), &[ta, t1]);
    }

    #[test]
    fn intersection_negation_distributes_over_union() {
        let mut db = setup_db();
        db.write_dedented(
            "/src/module.py",
            r#"
            class A: ...
            class B: ...
            "#,
        )
        .unwrap();
        let module = ruff_db::files::system_path_to_file(&db, "/src/module.py").unwrap();

        let a = global_symbol(&db, module, "A")
            .expect_type()
            .to_instance(&db);
        let b = global_symbol(&db, module, "B")
            .expect_type()
            .to_instance(&db);

        // intersection: A & B
        let intersection = IntersectionBuilder::new(&db)
            .add_positive(a)
            .add_positive(b)
            .build()
            .expect_intersection();
        assert_eq!(intersection.pos_vec(&db), &[a, b]);
        assert_eq!(intersection.neg_vec(&db), &[]);

        // ~intersection => ~A | ~B
        let negated_intersection = IntersectionBuilder::new(&db)
            .add_negative(Type::Intersection(intersection))
            .build()
            .expect_union();

        // should have as elements ~A and ~B
        let not_a = a.negate(&db);
        let not_b = b.negate(&db);
        assert_eq!(negated_intersection.elements(&db), &[not_a, not_b]);
    }

    #[test]
    fn mixed_intersection_negation_distributes_over_union() {
        let mut db = setup_db();
        db.write_dedented(
            "/src/module.py",
            r#"
            class A: ...
            class B: ...
            "#,
        )
        .unwrap();
        let module = ruff_db::files::system_path_to_file(&db, "/src/module.py").unwrap();

        let a = global_symbol(&db, module, "A")
            .expect_type()
            .to_instance(&db);
        let b = global_symbol(&db, module, "B")
            .expect_type()
            .to_instance(&db);
        let int = KnownClass::Int.to_instance(&db);

        // a_not_b: A & ~B
        let a_not_b = IntersectionBuilder::new(&db)
            .add_positive(a)
            .add_negative(b)
            .build()
            .expect_intersection();
        assert_eq!(a_not_b.pos_vec(&db), &[a]);
        assert_eq!(a_not_b.neg_vec(&db), &[b]);

        // let's build
        //         int & ~(A & ~B)
        //       = int & ~(A & ~B)
        //       = int & (~A | B)
        //       = (int & ~A) | (int & B)
        let t = IntersectionBuilder::new(&db)
            .add_positive(int)
            .add_negative(Type::Intersection(a_not_b))
            .build();
        assert_eq!(t.display(&db).to_string(), "int & ~A | int & B");
    }

    #[test]
    fn build_intersection_self_negation() {
        let db = setup_db();
        let ty = IntersectionBuilder::new(&db)
            .add_positive(Type::none(&db))
            .add_negative(Type::none(&db))
            .build();

        assert_eq!(ty, Type::Never);
    }

    #[test]
    fn build_intersection_simplify_negative_never() {
        let db = setup_db();
        let ty = IntersectionBuilder::new(&db)
            .add_positive(Type::none(&db))
            .add_negative(Type::Never)
            .build();

        assert_eq!(ty, Type::none(&db));
    }

    #[test]
    fn build_intersection_simplify_positive_never() {
        let db = setup_db();
        let ty = IntersectionBuilder::new(&db)
            .add_positive(Type::none(&db))
            .add_positive(Type::Never)
            .build();

        assert_eq!(ty, Type::Never);
    }

    #[test]
    fn build_intersection_simplify_negative_none() {
        let db = setup_db();

        let ty = IntersectionBuilder::new(&db)
            .add_negative(Type::none(&db))
            .add_positive(Type::IntLiteral(1))
            .build();
        assert_eq!(ty, Type::IntLiteral(1));

        let ty = IntersectionBuilder::new(&db)
            .add_positive(Type::IntLiteral(1))
            .add_negative(Type::none(&db))
            .build();
        assert_eq!(ty, Type::IntLiteral(1));
    }

    #[test]
    fn build_negative_union_de_morgan() {
        let db = setup_db();

        let union = UnionBuilder::new(&db)
            .add(Type::IntLiteral(1))
            .add(Type::IntLiteral(2))
            .build();
        assert_eq!(union.display(&db).to_string(), "Literal[1, 2]");

        let ty = IntersectionBuilder::new(&db).add_negative(union).build();

        let expected = IntersectionBuilder::new(&db)
            .add_negative(Type::IntLiteral(1))
            .add_negative(Type::IntLiteral(2))
            .build();

        assert_eq!(ty.display(&db).to_string(), "~Literal[1] & ~Literal[2]");
        assert_eq!(ty, expected);
    }

    #[test]
    fn build_intersection_simplify_positive_type_and_positive_subtype() {
        let db = setup_db();

        let t = KnownClass::Str.to_instance(&db);
        let s = Type::LiteralString;

        let ty = IntersectionBuilder::new(&db)
            .add_positive(t)
            .add_positive(s)
            .build();
        assert_eq!(ty, s);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(s)
            .add_positive(t)
            .build();
        assert_eq!(ty, s);

        let literal = Type::string_literal(&db, "a");
        let expected = IntersectionBuilder::new(&db)
            .add_positive(s)
            .add_negative(literal)
            .build();

        let ty = IntersectionBuilder::new(&db)
            .add_positive(t)
            .add_negative(literal)
            .add_positive(s)
            .build();
        assert_eq!(ty, expected);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(s)
            .add_negative(literal)
            .add_positive(t)
            .build();
        assert_eq!(ty, expected);
    }

    #[test]
    fn build_intersection_simplify_negative_type_and_negative_subtype() {
        let db = setup_db();

        let t = KnownClass::Str.to_instance(&db);
        let s = Type::LiteralString;

        let expected = IntersectionBuilder::new(&db).add_negative(t).build();

        let ty = IntersectionBuilder::new(&db)
            .add_negative(t)
            .add_negative(s)
            .build();
        assert_eq!(ty, expected);

        let ty = IntersectionBuilder::new(&db)
            .add_negative(s)
            .add_negative(t)
            .build();
        assert_eq!(ty, expected);

        let object = KnownClass::Object.to_instance(&db);
        let expected = IntersectionBuilder::new(&db)
            .add_negative(t)
            .add_positive(object)
            .build();

        let ty = IntersectionBuilder::new(&db)
            .add_negative(t)
            .add_positive(object)
            .add_negative(s)
            .build();
        assert_eq!(ty, expected);
    }

    #[test]
    fn build_intersection_simplify_negative_type_and_multiple_negative_subtypes() {
        let db = setup_db();

        let s1 = Type::IntLiteral(1);
        let s2 = Type::IntLiteral(2);
        let t = KnownClass::Int.to_instance(&db);

        let expected = IntersectionBuilder::new(&db).add_negative(t).build();

        let ty = IntersectionBuilder::new(&db)
            .add_negative(s1)
            .add_negative(s2)
            .add_negative(t)
            .build();
        assert_eq!(ty, expected);
    }

    #[test]
    fn build_intersection_simplify_negative_type_and_positive_subtype() {
        let db = setup_db();

        let t = KnownClass::Str.to_instance(&db);
        let s = Type::LiteralString;

        let ty = IntersectionBuilder::new(&db)
            .add_negative(t)
            .add_positive(s)
            .build();
        assert_eq!(ty, Type::Never);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(s)
            .add_negative(t)
            .build();
        assert_eq!(ty, Type::Never);

        // This should also work in the presence of additional contributions:
        let ty = IntersectionBuilder::new(&db)
            .add_positive(KnownClass::Object.to_instance(&db))
            .add_negative(t)
            .add_positive(s)
            .build();
        assert_eq!(ty, Type::Never);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(s)
            .add_negative(Type::string_literal(&db, "a"))
            .add_negative(t)
            .build();
        assert_eq!(ty, Type::Never);
    }

    #[test]
    fn build_intersection_simplify_disjoint_positive_types() {
        let db = setup_db();

        let t1 = Type::IntLiteral(1);
        let t2 = Type::none(&db);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(t1)
            .add_positive(t2)
            .build();
        assert_eq!(ty, Type::Never);

        // If there are any negative contributions, they should
        // be removed too.
        let ty = IntersectionBuilder::new(&db)
            .add_positive(KnownClass::Str.to_instance(&db))
            .add_negative(Type::LiteralString)
            .add_positive(t2)
            .build();
        assert_eq!(ty, Type::Never);
    }

    #[test]
    fn build_intersection_simplify_disjoint_positive_and_negative_types() {
        let db = setup_db();

        let t_p = KnownClass::Int.to_instance(&db);
        let t_n = Type::string_literal(&db, "t_n");

        let ty = IntersectionBuilder::new(&db)
            .add_positive(t_p)
            .add_negative(t_n)
            .build();
        assert_eq!(ty, t_p);

        let ty = IntersectionBuilder::new(&db)
            .add_negative(t_n)
            .add_positive(t_p)
            .build();
        assert_eq!(ty, t_p);

        let int_literal = Type::IntLiteral(1);
        let expected = IntersectionBuilder::new(&db)
            .add_positive(t_p)
            .add_negative(int_literal)
            .build();

        let ty = IntersectionBuilder::new(&db)
            .add_positive(t_p)
            .add_negative(int_literal)
            .add_negative(t_n)
            .build();
        assert_eq!(ty, expected);

        let ty = IntersectionBuilder::new(&db)
            .add_negative(t_n)
            .add_negative(int_literal)
            .add_positive(t_p)
            .build();
        assert_eq!(ty, expected);
    }

    #[test_case(Type::BooleanLiteral(true))]
    #[test_case(Type::BooleanLiteral(false))]
    #[test_case(Type::AlwaysTruthy)]
    #[test_case(Type::AlwaysFalsy)]
    fn build_intersection_simplify_split_bool(t_splitter: Type) {
        let db = setup_db();
        let bool_value = t_splitter.bool(&db) == Truthiness::AlwaysTrue;

        // We add t_object in various orders (in first or second position) in
        // the tests below to ensure that the boolean simplification eliminates
        // everything from the intersection, not just `bool`.
        let t_object = KnownClass::Object.to_instance(&db);
        let t_bool = KnownClass::Bool.to_instance(&db);

        let ty = IntersectionBuilder::new(&db)
            .add_positive(t_object)
            .add_positive(t_bool)
            .add_negative(t_splitter)
            .build();
        assert_eq!(ty, Type::BooleanLiteral(!bool_value));

        let ty = IntersectionBuilder::new(&db)
            .add_positive(t_bool)
            .add_positive(t_object)
            .add_negative(t_splitter)
            .build();
        assert_eq!(ty, Type::BooleanLiteral(!bool_value));

        let ty = IntersectionBuilder::new(&db)
            .add_positive(t_object)
            .add_negative(t_splitter)
            .add_positive(t_bool)
            .build();
        assert_eq!(ty, Type::BooleanLiteral(!bool_value));

        let ty = IntersectionBuilder::new(&db)
            .add_negative(t_splitter)
            .add_positive(t_object)
            .add_positive(t_bool)
            .build();
        assert_eq!(ty, Type::BooleanLiteral(!bool_value));
    }

    #[test_case(Type::Any)]
    #[test_case(Type::Unknown)]
    #[test_case(todo_type!())]
    fn build_intersection_t_and_negative_t_does_not_simplify(ty: Type) {
        let db = setup_db();

        let result = IntersectionBuilder::new(&db)
            .add_positive(ty)
            .add_negative(ty)
            .build();
        assert_eq!(result, ty);

        let result = IntersectionBuilder::new(&db)
            .add_negative(ty)
            .add_positive(ty)
            .build();
        assert_eq!(result, ty);
    }

    #[test]
    fn build_intersection_of_two_unions_simplify() {
        let mut db = setup_db();
        db.write_dedented(
            "/src/module.py",
            "
            class A: ...
            class B: ...
            a = A()
            b = B()
        ",
        )
        .unwrap();

        let file = system_path_to_file(&db, "src/module.py").expect("file to exist");

        let a = global_symbol(&db, file, "a").expect_type();
        let b = global_symbol(&db, file, "b").expect_type();
        let union = UnionBuilder::new(&db).add(a).add(b).build();
        assert_eq!(union.display(&db).to_string(), "A | B");
        let reversed_union = UnionBuilder::new(&db).add(b).add(a).build();
        assert_eq!(reversed_union.display(&db).to_string(), "B | A");
        let intersection = IntersectionBuilder::new(&db)
            .add_positive(union)
            .add_positive(reversed_union)
            .build();
        assert_eq!(intersection.display(&db).to_string(), "B | A");
    }

    #[test]
    fn build_union_of_two_intersections_simplify() {
        let mut db = setup_db();
        db.write_dedented(
            "/src/module.py",
            "
            class A: ...
            class B: ...
            a = A()
            b = B()
        ",
        )
        .unwrap();

        let file = system_path_to_file(&db, "src/module.py").expect("file to exist");

        let a = global_symbol(&db, file, "a").expect_type();
        let b = global_symbol(&db, file, "b").expect_type();
        let intersection = IntersectionBuilder::new(&db)
            .add_positive(a)
            .add_positive(b)
            .build();
        let reversed_intersection = IntersectionBuilder::new(&db)
            .add_positive(b)
            .add_positive(a)
            .build();
        let union = UnionBuilder::new(&db)
            .add(intersection)
            .add(reversed_intersection)
            .build();
        assert_eq!(union.display(&db).to_string(), "A & B");
    }
}
