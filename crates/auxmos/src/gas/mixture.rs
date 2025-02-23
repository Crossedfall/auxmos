use itertools::{
	Either,
	EitherOrBoth::{Both, Left, Right},
	Itertools,
};

use atomic_float::AtomicF32;

use tinyvec::TinyVec;

use crate::reaction::{Reaction, ReactionPriority};

use super::{
	constants::*, gas_visibility, total_num_gases, with_reactions, with_specific_heats, GasIDX,
};

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use std::collections::BTreeMap;

type SpecificFireInfo = (usize, f32, f32);

struct GasCache(AtomicF32);

impl Clone for GasCache {
	fn clone(&self) -> Self {
		Self(AtomicF32::new(self.0.load(Relaxed)))
	}
}

impl Default for GasCache {
	fn default() -> Self {
		Self(AtomicF32::new(f32::NAN))
	}
}

impl GasCache {
	pub fn invalidate(&self) {
		self.0.store(f32::NAN, Relaxed);
	}
	//cannot fix this, because f is FnMut and then() takes FnOnce
	#[allow(clippy::redundant_closure)]
	pub fn get_or_else(&self, mut f: impl FnMut() -> f32) -> f32 {
		match self
			.0
			.fetch_update(Relaxed, Relaxed, |x| x.is_nan().then(|| f()))
		{
			Ok(_) => self.0.load(Relaxed),
			Err(x) => x,
		}
	}
	pub fn set(&self, v: f32) {
		self.0.store(v, Relaxed);
	}
}

pub fn visibility_step(gas_amt: f32) -> u32 {
	(gas_amt / MOLES_GAS_VISIBLE_STEP)
		.ceil()
		.min(FACTOR_GAS_VISIBLE_MAX)
		.max(1.0) as u32
}

/// The data structure representing a Space Station 13 gas mixture.
/// Unlike Monstermos, this doesn't have the archive built-in; instead,
/// the archive is a feature of the turf grid, only existing during
/// turf processing.
/// Also missing is `last_share`; due to the usage of Rust,
/// processing no longer requires sleeping turfs. Instead, we're using
/// a proper, fully-simulated FDM system, much like LINDA but without
/// sleeping turfs.
#[derive(Clone)]
pub struct Mixture {
	temperature: f32,
	pub volume: f32,
	min_heat_capacity: f32,
	moles: TinyVec<[f32; 8]>,
	cached_heat_capacity: GasCache,
	immutable: bool,
}

impl Default for Mixture {
	fn default() -> Self {
		Self::new()
	}
}

impl Mixture {
	/// Makes an empty gas mixture.
	#[must_use]
	pub fn new() -> Self {
		Self {
			moles: TinyVec::new(),
			temperature: 2.7,
			volume: 2500.0,
			min_heat_capacity: 0.0,
			immutable: false,
			cached_heat_capacity: GasCache::default(),
		}
	}
	/// Makes an empty gas mixture with the given volume.
	#[must_use]
	pub fn from_vol(vol: f32) -> Self {
		let mut ret = Self::new();
		ret.volume = vol;
		ret
	}
	/// Returns if any data is corrupt.
	pub fn is_corrupt(&self) -> bool {
		!self.temperature.is_normal() || self.moles.len() > total_num_gases()
	}
	/// Fixes any corruption found.
	pub fn fix_corruption(&mut self) {
		self.garbage_collect();
		if self.temperature < 2.7 || !self.temperature.is_normal() {
			self.set_temperature(293.15);
		}
	}
	/// Returns the temperature of the mix. T
	pub fn get_temperature(&self) -> f32 {
		self.temperature
	}
	/// Sets the temperature, if the mix isn't immutable. T
	pub fn set_temperature(&mut self, temp: f32) {
		if !self.immutable && temp.is_normal() {
			self.temperature = temp;
		}
	}
	/// Sets the minimum heat capacity of this mix.
	pub fn set_min_heat_capacity(&mut self, amt: f32) {
		self.min_heat_capacity = amt;
	}
	/// Returns an iterator over the gas keys and mole amounts thereof.
	pub fn enumerate(&self) -> impl Iterator<Item = (GasIDX, f32)> + '_ {
		self.moles.iter().copied().enumerate()
	}
	/// Allows closures to iterate over each gas.
	/// # Errors
	/// If the closure errors.
	pub fn for_each_gas(
		&self,
		mut f: impl FnMut(GasIDX, f32) -> Result<(), auxtools::Runtime>,
	) -> Result<(), auxtools::Runtime> {
		for (i, g) in self.enumerate() {
			f(i, g)?;
		}
		Ok(())
	}
	/// As `for_each_gas`, but with mut refs to the mole counts instead of copies.
	/// # Errors
	/// If the closure errors.
	pub fn for_each_gas_mut(
		&mut self,
		mut f: impl FnMut(GasIDX, &mut f32) -> Result<(), auxtools::Runtime>,
	) -> Result<(), auxtools::Runtime> {
		for (i, g) in self.moles.iter_mut().enumerate() {
			f(i, g)?;
		}
		Ok(())
	}
	/// Returns (by value) the amount of moles of a given index the mix has. M
	pub fn get_moles(&self, idx: GasIDX) -> f32 {
		self.moles.get(idx).copied().unwrap_or(0.0)
	}
	/// Sets the mix to be internally immutable. Rust doesn't know about any of this, obviously.
	pub fn mark_immutable(&mut self) {
		self.immutable = true;
	}
	/// Returns whether this gas mixture is immutable.
	pub fn is_immutable(&self) -> bool {
		self.immutable
	}
	fn maybe_expand(&mut self, size: usize) {
		if self.moles.len() < size {
			self.moles.resize(size, 0.0);
		}
	}
	/// If mix is not immutable, sets the gas at the given `idx` to the given `amt`.
	pub fn set_moles(&mut self, idx: GasIDX, amt: f32) {
		if !self.immutable
			&& idx < total_num_gases()
			&& (idx <= self.moles.len() || (amt > GAS_MIN_MOLES && amt.is_normal()))
		{
			self.maybe_expand((idx + 1) as usize);
			unsafe {
				*self.moles.get_unchecked_mut(idx) = amt;
			};
			self.cached_heat_capacity.invalidate();
		}
	}
	pub fn adjust_moles(&mut self, idx: GasIDX, amt: f32) {
		if !self.immutable && amt.is_normal() && idx < total_num_gases() {
			self.maybe_expand((idx + 1) as usize);
			let r = unsafe { self.moles.get_unchecked_mut(idx) };
			*r += amt;
			if amt <= 0.0 {
				self.garbage_collect();
			}
			self.cached_heat_capacity.invalidate();
		}
	}
	pub fn adjust_multi(&mut self, adjustments: &[(usize, f32)]) {
		if !self.immutable {
			let num_gases = total_num_gases();
			self.maybe_expand(
				adjustments
					.iter()
					.filter_map(|&(i, _)| (i < num_gases).then(|| i))
					.max()
					.unwrap_or(0) + 1,
			);
			let mut dirty = false;
			let mut should_collect = false;
			for (idx, amt) in adjustments {
				if *idx < num_gases && amt.is_normal() {
					let r = unsafe { self.moles.get_unchecked_mut(*idx) };
					*r += *amt;
					if *amt <= 0.0 {
						should_collect = true;
					}
					dirty = true;
				}
			}
			if dirty {
				self.cached_heat_capacity.invalidate();
			}
			if should_collect {
				self.garbage_collect();
			}
		}
	}
	#[inline(never)] // mostly this makes it so that heat_capacity itself is inlined
	fn slow_heat_capacity(&self) -> f32 {
		with_specific_heats(|heats| {
			self.moles
				.iter()
				.copied()
				.zip(heats.iter())
				.fold(0.0, |acc, (amt, cap)| cap.mul_add(amt, acc))
		})
		.max(self.min_heat_capacity)
	}
	/// The heat capacity of the material. [joules?]/mole-kelvin.
	pub fn heat_capacity(&self) -> f32 {
		self.cached_heat_capacity
			.get_or_else(|| self.slow_heat_capacity())
	}
	/// Heat capacity of exactly one gas in this mix.
	pub fn partial_heat_capacity(&self, idx: GasIDX) -> f32 {
		self.moles
			.get(idx)
			.filter(|amt| amt.is_normal())
			.map_or(0.0, |amt| amt * with_specific_heats(|heats| heats[idx]))
	}
	/// The total mole count of the mixture. Moles.
	pub fn total_moles(&self) -> f32 {
		self.moles.iter().sum()
	}
	/// Pressure. Kilopascals.
	pub fn return_pressure(&self) -> f32 {
		self.total_moles() * R_IDEAL_GAS_EQUATION * self.temperature / self.volume
	}
	/// Thermal energy. Joules?
	pub fn thermal_energy(&self) -> f32 {
		self.heat_capacity() * self.temperature
	}
	/// Merges one gas mixture into another.
	pub fn merge(&mut self, giver: &Self) {
		if self.immutable {
			return;
		}
		let our_heat_capacity = self.heat_capacity();
		let other_heat_capacity = giver.heat_capacity();
		self.maybe_expand(giver.moles.len());
		for (a, b) in self.moles.iter_mut().zip(giver.moles.iter()) {
			*a += b;
		}
		let combined_heat_capacity = our_heat_capacity + other_heat_capacity;
		if combined_heat_capacity > MINIMUM_HEAT_CAPACITY {
			self.set_temperature(
				(our_heat_capacity * self.temperature + other_heat_capacity * giver.temperature)
					/ (combined_heat_capacity),
			);
		}
		self.cached_heat_capacity.set(combined_heat_capacity);
	}
	/// Transfers only the given gases from us to another mix.
	pub fn transfer_gases_to(&mut self, r: f32, gases: &[GasIDX], into: &mut Self) {
		let ratio = r.clamp(0.0, 1.0);
		let initial_energy = into.thermal_energy();
		let mut heat_transfer = 0.0;
		with_specific_heats(|heats| {
			for i in gases.iter().copied() {
				if let Some(orig) = self.moles.get_mut(i) {
					let delta = *orig * ratio;
					heat_transfer += delta * self.temperature * heats[i];
					*orig -= delta;
					into.adjust_moles(i, delta);
				}
			}
		});
		self.cached_heat_capacity.invalidate();
		into.cached_heat_capacity.invalidate();
		into.set_temperature((initial_energy + heat_transfer) / into.heat_capacity());
	}
	/// Takes a percentage of this gas mixture's moles and puts it into another mixture. if this mix is mutable, also removes those moles from the original.
	pub fn remove_ratio_into(&mut self, mut ratio: f32, into: &mut Self) {
		if ratio <= 0.0 {
			return;
		}
		if ratio >= 1.0 {
			ratio = 1.0;
		}
		into.copy_from_mutable(self);
		into.multiply(ratio);
		self.multiply(1.0 - ratio);
	}
	/// As `remove_ratio_into`, but a raw number of moles instead of a ratio.
	pub fn remove_into(&mut self, amount: f32, into: &mut Self) {
		self.remove_ratio_into(amount / self.total_moles(), into);
	}
	/// A convenience function that makes the mixture for `remove_ratio_into` on the spot and returns it.
	#[must_use]
	pub fn remove_ratio(&mut self, ratio: f32) -> Self {
		let mut removed = Self::from_vol(self.volume);
		self.remove_ratio_into(ratio, &mut removed);
		removed
	}
	/// Like `remove_ratio`, but with moles.
	#[must_use]
	pub fn remove(&mut self, amount: f32) -> Self {
		self.remove_ratio(amount / self.total_moles())
	}
	/// Copies from a given gas mixture, if we're mutable.
	pub fn copy_from_mutable(&mut self, sample: &Self) {
		if self.immutable {
			return;
		}
		self.moles = sample.moles.clone();
		self.temperature = sample.temperature;
		self.cached_heat_capacity = sample.cached_heat_capacity.clone();
	}
	/// A very simple finite difference solution to the heat transfer equation.
	/// Works well enough for our purposes, though perhaps called less often
	/// than it ought to be while we're working in Rust.
	/// Differs from the original by not using archive, since we don't put the archive into the gas mix itself anymore.
	pub fn temperature_share(&mut self, sharer: &mut Self, conduction_coefficient: f32) -> f32 {
		let temperature_delta = self.temperature - sharer.temperature;
		if temperature_delta.abs() > MINIMUM_TEMPERATURE_DELTA_TO_CONSIDER {
			let self_heat_capacity = self.heat_capacity();
			let sharer_heat_capacity = sharer.heat_capacity();

			if sharer_heat_capacity > MINIMUM_HEAT_CAPACITY
				&& self_heat_capacity > MINIMUM_HEAT_CAPACITY
			{
				let heat = conduction_coefficient
					* temperature_delta * (self_heat_capacity * sharer_heat_capacity
					/ (self_heat_capacity + sharer_heat_capacity));
				if !self.immutable {
					self.set_temperature((self.temperature - heat / self_heat_capacity).max(TCMB));
				}
				if !sharer.immutable {
					sharer.set_temperature(
						(sharer.temperature + heat / sharer_heat_capacity).max(TCMB),
					);
				}
			}
		}
		sharer.temperature
	}
	/// As above, but you may put in any arbitrary coefficient, temp, heat capacity.
	/// Only used for superconductivity as of right now.
	pub fn temperature_share_non_gas(
		&mut self,
		conduction_coefficient: f32,
		sharer_temperature: f32,
		sharer_heat_capacity: f32,
	) -> f32 {
		let temperature_delta = self.temperature - sharer_temperature;
		if temperature_delta.abs() > MINIMUM_TEMPERATURE_DELTA_TO_CONSIDER {
			let self_heat_capacity = self.heat_capacity();

			if sharer_heat_capacity > MINIMUM_HEAT_CAPACITY
				&& self_heat_capacity > MINIMUM_HEAT_CAPACITY
			{
				let heat = conduction_coefficient
					* temperature_delta * (self_heat_capacity * sharer_heat_capacity
					/ (self_heat_capacity + sharer_heat_capacity));
				if !self.immutable {
					self.set_temperature((self.temperature - heat / self_heat_capacity).max(TCMB));
				}
				return (sharer_temperature + heat / sharer_heat_capacity).max(TCMB);
			}
		}
		sharer_temperature
	}
	/// The second part of old compare(). Compares temperature, but only if this gas has sufficiently high moles.
	pub fn temperature_compare(&self, sample: &Self) -> bool {
		(self.get_temperature() - sample.get_temperature()).abs()
			> MINIMUM_TEMPERATURE_DELTA_TO_SUSPEND
			&& (self.total_moles() > MINIMUM_MOLES_DELTA_TO_MOVE)
	}
	/// Returns the maximum mole delta for an individual gas.
	pub fn compare(&self, sample: &Self) -> f32 {
		self.moles
			.iter()
			.copied()
			.zip_longest(sample.moles.iter().copied())
			.fold(0.0, |acc, pair| acc.max(pair.reduce(|a, b| (b - a).abs())))
	}
	pub fn compare_with(&self, sample: &Self, amt: f32) -> bool {
		self.moles
			.as_slice()
			.iter()
			.zip_longest(sample.moles.as_slice().iter())
			.rev()
			.any(|pair| match pair {
				Left(a) => a >= &amt,
				Right(b) => b >= &amt,
				Both(a, b) => (a - b).abs() >= amt,
			})
	}
	/// Clears the moles from the gas.
	pub fn clear(&mut self) {
		if !self.immutable {
			self.moles.clear();
			self.cached_heat_capacity.invalidate();
		}
	}
	/// Resets the gas mixture to an initialized-with-volume state.
	pub fn clear_with_vol(&mut self, vol: f32) {
		self.temperature = 2.7;
		self.volume = vol;
		self.min_heat_capacity = 0.0;
		self.immutable = false;
		self.clear();
	}
	/// Multiplies every gas molage with this value.
	pub fn multiply(&mut self, multiplier: f32) {
		if !self.immutable {
			for amt in self.moles.iter_mut() {
				*amt *= multiplier;
			}
			self.cached_heat_capacity.invalidate();
			self.garbage_collect();
		}
	}
	pub fn add(&mut self, num: f32) {
		if !self.immutable {
			for amt in self.moles.iter_mut() {
				*amt += num;
			}
			self.cached_heat_capacity.invalidate();
			self.garbage_collect();
		}
	}
	pub fn can_react_with_reactions(
		&self,
		reactions: &BTreeMap<ReactionPriority, Reaction>,
	) -> bool {
		//priorities are inversed because fuck you
		reactions
			.values()
			.rev()
			.any(|reaction| reaction.check_conditions(self))
	}
	/// Checks if the proc can react with any reactions.
	pub fn can_react(&self) -> bool {
		with_reactions(|reactions| self.can_react_with_reactions(reactions))
	}
	pub fn all_reactable_with_slice(
		&self,
		reactions: &BTreeMap<ReactionPriority, Reaction>,
	) -> TinyVec<[u64; MAX_REACTION_TINYVEC_SIZE]> {
		//priorities are inversed because fuck you
		reactions
			.values()
			.rev()
			.filter_map(|thin| thin.check_conditions(self).then(|| thin.get_id()))
			.collect()
	}
	/// Gets all of the reactions this mix should do.
	pub fn all_reactable(&self) -> TinyVec<[u64; MAX_REACTION_TINYVEC_SIZE]> {
		with_reactions(|reactions| self.all_reactable_with_slice(reactions))
	}
	/// Returns a tuple with oxidation power and fuel amount of this gas mixture.
	pub fn get_burnability(&self) -> (f32, f32) {
		use crate::types::FireInfo;
		super::with_gas_info(|gas_info| {
			self.moles
				.iter()
				.zip(gas_info)
				.fold((0.0, 0.0), |mut acc, (&amt, this_gas_info)| {
					if amt > GAS_MIN_MOLES {
						match this_gas_info.fire_info {
							FireInfo::Oxidation(oxidation) => {
								if self.temperature > oxidation.temperature() {
									let amount = amt
										* (1.0 - oxidation.temperature() / self.temperature)
											.max(0.0);
									acc.0 += amount * oxidation.power();
								}
							}
							FireInfo::Fuel(fire) => {
								if self.temperature > fire.temperature() {
									let amount = amt
										* (1.0 - fire.temperature() / self.temperature).max(0.0);
									acc.1 += amount / fire.burn_rate();
								}
							}
							FireInfo::None => (),
						}
					}
					acc
				})
		})
	}
	/// Returns only the oxidation power. Since this calculates burnability anyway, prefer `get_burnability`.
	pub fn get_oxidation_power(&self) -> f32 {
		self.get_burnability().0
	}
	/// Returns only fuel amount. Since this calculates burnability anyway, prefer `get_burnability`.
	pub fn get_fuel_amount(&self) -> f32 {
		self.get_burnability().1
	}
	/// Like `get_fire_info`, but takes a reference to a gas info vector,
	/// so one doesn't need to do a recursive lock on the global list.
	pub fn get_fire_info_with_lock(
		&self,
		gas_info: &[super::GasType],
	) -> (Vec<SpecificFireInfo>, Vec<SpecificFireInfo>) {
		use crate::types::FireInfo;
		self.moles
			.iter()
			.zip(gas_info)
			.enumerate()
			.filter_map(|(i, (&amt, this_gas_info))| {
				(amt > GAS_MIN_MOLES)
					.then(|| match this_gas_info.fire_info {
						FireInfo::Oxidation(oxidation) => (self.get_temperature()
							> oxidation.temperature())
						.then(|| {
							let amount = amt
								* (1.0 - oxidation.temperature() / self.get_temperature()).max(0.0);
							Either::Right((i, amount, amount * oxidation.power()))
						}),
						FireInfo::Fuel(fuel) => {
							(self.get_temperature() > fuel.temperature()).then(|| {
								let amount = amt
									* (1.0 - fuel.temperature() / self.get_temperature()).max(0.0);
								Either::Left((i, amount, amount / fuel.burn_rate()))
							})
						}
						FireInfo::None => None,
					})
					.flatten()
			})
			.partition_map(|r| r)
	}
	/// Returns two vectors:
	/// The first contains all oxidizers in this list, as well as their actual mole amounts and how much fuel they can oxidize.
	/// The second contains all fuel sources in this list, as well as their actual mole amounts and how much oxidizer they can react with.
	pub fn get_fire_info(&self) -> (Vec<SpecificFireInfo>, Vec<SpecificFireInfo>) {
		super::with_gas_info(|gas_info| self.get_fire_info_with_lock(gas_info))
	}
	/// Adds heat directly to the gas mixture, in joules (probably).
	pub fn adjust_heat(&mut self, heat: f32) {
		let cap = self.heat_capacity();
		self.set_temperature(((cap * self.temperature) + heat) / cap);
	}
	/// Returns true if there's a visible gas in this mix.
	pub fn is_visible(&self) -> bool {
		self.enumerate()
			.any(|(i, gas)| gas_visibility(i as usize).map_or(false, |amt| gas >= amt))
	}
	pub fn vis_hash(&self, gas_visibility: &[Option<f32>]) -> u64 {
		use std::hash::Hasher;
		let mut hasher: ahash::AHasher = ahash::AHasher::default();
		for (i, gas_amt) in self.enumerate() {
			if unsafe { gas_visibility.get_unchecked(i) }
				.filter(|&amt| gas_amt > amt)
				.is_some()
			{
				hasher.write_usize(i);
				hasher.write_usize(visibility_step(gas_amt) as usize)
			}
		}
		hasher.finish()
	}
	/// Compares the current vis hash to the provided one; returns true if they are
	pub fn vis_hash_changed(
		&self,
		gas_visibility: &[Option<f32>],
		hash_holder: &AtomicU64,
	) -> bool {
		let cur_hash = self.vis_hash(gas_visibility);
		hash_holder
			.fetch_update(Relaxed, Relaxed, |item| {
				(item != cur_hash).then(|| cur_hash)
			})
			.is_ok()
	}
	// Removes all redundant zeroes from the gas mixture.
	pub fn garbage_collect(&mut self) {
		let mut last_valid_found = 0;
		for (i, amt) in self.moles.iter_mut().enumerate() {
			if *amt > GAS_MIN_MOLES {
				last_valid_found = i;
			} else {
				*amt = 0.0;
			}
		}
		self.moles.truncate(last_valid_found + 1);
	}
}

use std::ops::{Add, Mul};

/// Takes a copy of the mix, merges the right hand side, then returns the copy.
impl Add<&Mixture> for Mixture {
	type Output = Self;

	fn add(self, rhs: &Mixture) -> Self {
		let mut ret = self;
		ret.merge(rhs);
		ret
	}
}

/// Takes a copy of the mix, merges the right hand side, then returns the copy.
impl<'a, 'b> Add<&'a Mixture> for &'b Mixture {
	type Output = Mixture;

	fn add(self, rhs: &Mixture) -> Mixture {
		let mut ret = self.clone();
		ret.merge(rhs);
		ret
	}
}

/// Makes a copy of the given mix, multiplied by a scalar.
impl Mul<f32> for Mixture {
	type Output = Self;

	fn mul(self, rhs: f32) -> Self {
		let mut ret = self;
		ret.multiply(rhs);
		ret
	}
}

/// Makes a copy of the given mix, multiplied by a scalar.
impl<'a> Mul<f32> for &'a Mixture {
	type Output = Mixture;

	fn mul(self, rhs: f32) -> Mixture {
		let mut ret = self.clone();
		ret.multiply(rhs);
		ret
	}
}

impl PartialEq for Mixture {
	fn eq(&self, other: &Self) -> bool {
		self.moles.len() == other.moles.len()
			&& self.temperature == other.temperature
			&& self
				.moles
				.iter()
				.zip(other.moles.iter())
				.all(|(a, b)| (a - b).abs() < GAS_MIN_MOLES)
	}
}

impl Eq for Mixture {}

#[cfg(test)]
mod tests {

	use super::*;
	use crate::gas::types::{destroy_gas_statics, register_gas_manually, set_gas_statics_manually};

	fn initialize_gases() {
		set_gas_statics_manually();
		register_gas_manually("o2", 20.0);
		register_gas_manually("n2", 20.0);
		register_gas_manually("n2o", 20.0);
	}

	#[test]
	fn test_merge() {
		initialize_gases();
		let mut into = Mixture::new();
		into.set_moles(0, 82.0);
		into.set_moles(1, 22.0);
		into.set_temperature(293.15);
		let mut source = Mixture::new();
		source.set_moles(3, 100.0);
		source.set_temperature(313.15);
		into.merge(&source);
		// make sure that the merge successfuly moved the moles
		assert_eq!(into.get_moles(3), 100.0);
		assert_eq!(source.get_moles(3), 100.0); // source is not modified by merge
										/*
										make sure that the merge successfuly changed the temperature of the mix merged into:
										test gases have heat capacities of (82 * 20 + 22 * 20) and (100 * 20) respectively, so total thermal energies of
										(82 * 20 + 22 * 20) * 293.15 and (100 * 20) * 313.15 respectively once multiplied by temperatures. add those together,
										then divide by new total heat capacity:
										(609,752 + 626,300)/(2,080 + 2,000) =
										~
										302.953
										so we compare to see if it's relatively close to 302.953, cause of floating point precision
										*/
		assert!(
			(into.get_temperature() - 302.953).abs() < 0.01,
			"{} should be near 302.953, is {}",
			into.get_temperature(),
			(into.get_temperature() - 302.953)
		);
		destroy_gas_statics();
	}
	#[test]
	fn test_remove() {
		initialize_gases();
		// also tests multiply, copy_from_mutable
		let mut removed = Mixture::new();
		removed.set_moles(0, 22.0);
		removed.set_moles(1, 82.0);
		let new = removed.remove_ratio(0.5);
		assert_eq!(removed.compare(&new) >= MINIMUM_MOLES_DELTA_TO_MOVE, false);
		assert_eq!(removed.get_moles(0), 11.0);
		assert_eq!(removed.get_moles(1), 41.0);
		removed.mark_immutable();
		let new_two = removed.remove_ratio(0.5);
		assert_eq!(
			removed.compare(&new_two) >= MINIMUM_MOLES_DELTA_TO_MOVE,
			true
		);
		assert_eq!(removed.get_moles(0), 11.0);
		assert_eq!(removed.get_moles(1), 41.0);
		assert_eq!(new_two.get_moles(0), 5.5);
		destroy_gas_statics();
	}
}
