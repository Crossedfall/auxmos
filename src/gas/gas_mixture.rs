use itertools::Itertools;

use itertools::EitherOrBoth::{Both, Left, Right};

use std::cell::Cell;

use bitvec::prelude::*;

use super::reaction::ReactionIdentifier;

use super::constants::*;

use super::{gas_specific_heat, gas_visibility, total_num_gases, with_reactions};

fn get_bit_position<O: BitOrder, V: BitStore>(
	bitvec: &BitVec<O, V>,
	idx: usize,
) -> Result<usize, (usize, bool)> {
	let count = bitvec.get(..idx).unwrap_or_else(|| bitvec).count_ones();
	if bitvec.get(idx).map_or(false, |e| *e) {
		Ok(count)
	} else {
		Err((count, bitvec.len() <= idx))
	}
}

/// The data structure representing a Space Station 13 gas mixture.
/// Unlike Monstermos, this doesn't have the archive built-in; instead,
/// the archive is a feature of the turf grid, only existing during
/// turf processing.
/// Also missing is last_share; due to the usage of Rust,
/// processing no longer requires sleeping turfs. Instead, we're using
/// a proper, fully-simulated FDM system, much like LINDA but without
/// sleeping turfs.
#[derive(Clone)]
pub struct GasMixture {
	temperature: f32,
	pub volume: f32,
	min_heat_capacity: f32,
	immutable: bool,
	mole_ids: BitVec<Lsb0, u8>,
	moles: Vec<f32>,
	heat_capacities: Vec<f32>,
	cached_heat_capacity: Cell<Option<f32>>,
}

/*
	Cell is not thread-safe. However, we use it only for caching heat capacity. The worst case race condition
	is thus thread A and B try to access heat capacity at the same time; both find that it's currently
	uncached, so both go to calculate it; both calculate it, and both calculate it to the same value,
	then one sets the cache to that value, then the other does.

	Technically, a worse one would be thread A mutates the gas mixture, changing a gas amount,
	while thread B tries to get its heat capacity; thread B finds a well-defined heat capacity,
	which is not correct, and uses it for a calculation, but this cannot happen: thread A would
	have a write lock, precluding thread B from accessing it.
*/
unsafe impl Sync for GasMixture {}

impl Default for GasMixture {
	fn default() -> Self {
		Self::new()
	}
}

impl GasMixture {
	/// Makes an empty gas mixture.
	pub fn new() -> Self {
		GasMixture {
			mole_ids: BitVec::new(),
			moles: Vec::with_capacity(8),
			heat_capacities: Vec::with_capacity(8),
			temperature: 2.7,
			volume: 2500.0,
			min_heat_capacity: MINIMUM_HEAT_CAPACITY,
			immutable: false,
			cached_heat_capacity: Cell::new(None),
		}
	}
	/// Makes an empty gas mixture with the given volume.
	pub fn from_vol(vol: f32) -> Self {
		let mut ret: GasMixture = GasMixture::new();
		ret.volume = vol;
		ret
	}
	/// Makes a gas entry allocated by the arena.
	pub fn new_from_arena(vol: f32) -> GasMixtureAlloc {
		if let Some(idx) = super::GAS_MIXTURES.try_push(|g| g.clear_with_vol(vol)) {
			GasMixtureAlloc::Arena(idx)
		} else {
			GasMixtureAlloc::Heap(GasMixture::from_vol(vol))
		}
	}
	/// Returns an object that can be used to merge many gases into one, then average them.
	pub fn merger() -> GasSummer {
		GasSummer {
			cur_ids: BitVec::new(),
			cur_summed_counts: Vec::with_capacity(8),
			cur_summed_heat_cap: 0.0,
			cur_summed_heat: 0.0,
			cur_summed_vols: 0.0,
			cur_temp: -1.0,
		}
	}
	/// Returns if any data is corrupt.
	pub fn is_corrupt(&self) -> bool {
		!self.temperature.is_normal()
			|| (self.mole_ids.count_ones() != self.moles.len())
			|| (self.mole_ids.count_ones() != self.heat_capacities.len())
			|| self.moles.iter().any(|amt| !amt.is_normal())
			|| self.heat_capacities.iter().any(|cap| !cap.is_normal())
			|| self.mole_ids.iter_ones().any(|i| i > total_num_gases())
	}
	/// Fixes any corruption found.
	pub fn fix_corruption(&mut self) {
		self.mole_ids.truncate(total_num_gases());
		self.heat_capacities = self
			.mole_ids
			.iter_ones()
			.map(|i| gas_specific_heat(i))
			.collect();
		self.garbage_collect();
		if !self.temperature.is_normal() {
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
	pub fn enumerate(&self) -> impl Iterator<Item = (usize, &f32)> {
		self.mole_ids.iter_ones().zip(self.moles.iter())
	}
	/// Same as above, but with heat included.
	pub fn enumerate_with_heat(&self) -> impl Iterator<Item = (usize, &f32, &f32)> {
		self.enumerate()
			.zip(self.heat_capacities.iter())
			.map(|((a, b), c)| (a, b, c))
	}
	/// Allows closures to iterate over each gas.
	pub fn for_each_gas<F>(&self, f: F)
	where
		F: FnMut((usize, &f32)),
	{
		self.enumerate().for_each(f);
	}
	/// Returns a slice representing the non-zero gases in the mix.
	pub fn get_gases(&self) -> bitvec::slice::IterOnes<Lsb0, u8> {
		self.mole_ids.iter_ones()
	}
	/// Returns a bitfield of gases in the mix.
	pub fn gases_as_bitfield(&self) -> &BitSlice<Lsb0, u8> {
		&self.mole_ids
	}
	/// Returns (by value) the amount of moles of a given index the mix has. M
	pub fn get_moles(&self, idx: usize) -> f32 {
		if self.mole_ids.get(idx).map_or(false, |e| *e) {
			*unsafe {
				self.moles
					.get_unchecked(self.mole_ids.get_unchecked(..idx).count_ones())
			}
		// count_ones with the last bit chopped off always returns a value at most moles.len()-1,
		// because mole_ids is only added to when new moles are added
		} else {
			0.0
		}
	}
	/// Sets the mix to be internally immutable. Rust doesn't know about any of this, obviously.
	pub fn mark_immutable(&mut self) {
		self.immutable = true;
	}

	/// Returns whether this gas mixture is immutable.
	pub fn is_immutable(&self) -> bool {
		self.immutable
	}
	/// If mix is not immutable, sets the gas at the given `idx` to the given `amt`.
	pub fn set_moles(&mut self, idx: usize, amt: f32) {
		if !self.immutable && idx < total_num_gases() {
			if amt.is_normal() && amt > GAS_MIN_MOLES {
				match get_bit_position(&self.mole_ids, idx) {
					Ok(i) => {
						unsafe { *self.moles.get_unchecked_mut(i) = amt };
					}
					Err((i, resize)) => {
						if resize {
							self.mole_ids.resize(idx + 1, false);
						}
						self.mole_ids.set(idx, true);
						self.moles.insert(i, amt);
						self.heat_capacities.insert(i, gas_specific_heat(idx));
					}
				}
			} else {
				if let Ok(i) = get_bit_position(&self.mole_ids, idx) {
					self.moles.remove(i);
					self.mole_ids.set(idx, false);
					self.heat_capacities.remove(i);
				}
			}
		}
		self.cached_heat_capacity.set(None); // will be recalculated, this is the only time it's required (!!)
	}

	pub fn adjust_moles(&mut self, idx: usize, amt: f32) {
		if !self.immutable && amt.is_normal() {
			if let Some(i) = match get_bit_position(&self.mole_ids, idx) {
				Ok(i) => {
					let gas = unsafe { self.moles.get_unchecked_mut(i) };
					*gas += amt;
					*gas = gas.clamp(0.0, 1e31);
					if !gas.is_normal() || *gas <= GAS_MIN_MOLES {
						Some(i)
					} else {
						None
					}
				}
				Err((i, resize)) => {
					if amt > 0.0 {
						if resize {
							self.mole_ids.resize(idx + 1, false);
						}
						self.mole_ids.set(idx, true);
						self.moles.insert(i, amt);
						self.heat_capacities.insert(i, gas_specific_heat(idx));
					}
					None
				}
			} {
				self.moles.remove(i);
				self.mole_ids.set(idx, false);
				self.heat_capacities.remove(i);
			}
		}
	}
	/// The heat capacity of the material. [joules?]/mole-kelvin.
	pub fn heat_capacity(&self) -> f32 {
		if let Some(heat_cap) = self.cached_heat_capacity.get() {
			if heat_cap.is_normal() {
				return heat_cap;
			}
		}
		let heat_cap = self
			.enumerate_with_heat()
			.fold(0.0, |acc, (_, amt, &cap)| amt.mul_add(cap, acc))
			.max(self.min_heat_capacity);
		self.cached_heat_capacity.set(Some(heat_cap));
		heat_cap
	}
	/// Heat capacity of exactly one gas in this mix.
	pub fn partial_heat_capacity(&self, idx: usize) -> f32 {
		if let Ok(i) = get_bit_position(&self.mole_ids, idx) {
			unsafe { self.moles.get_unchecked(i) * self.heat_capacities.get_unchecked(i) }
		} else {
			0.0
		}
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
	pub fn merge(&mut self, giver: &GasMixture) {
		if self.immutable || giver.is_corrupt() {
			return;
		}
		let our_heat_capacity = self.heat_capacity();
		let other_heat_capacity = giver.heat_capacity();
		for (our_idx, pair) in self
			.mole_ids
			.iter_ones()
			.merge_join_by(giver.enumerate_with_heat(), |our_id, (giver_id, _, _)| {
				our_id.cmp(giver_id)
			})
			.enumerate()
		{
			match pair {
				Left(_) => (),
				Right((_, amt, cap)) => {
					self.moles.insert(our_idx, *amt);
					self.heat_capacities.insert(our_idx, *cap);
				}
				Both(_, (_, amt, _)) => unsafe { *self.moles.get_unchecked_mut(our_idx) += amt },
			}
		}
		self.mole_ids |= giver.mole_ids.clone();
		let combined_heat_capacity = our_heat_capacity + other_heat_capacity;
		if combined_heat_capacity > MINIMUM_HEAT_CAPACITY {
			self.set_temperature(
				(our_heat_capacity * self.temperature + other_heat_capacity * giver.temperature)
					/ (combined_heat_capacity),
			);
		}
		self.cached_heat_capacity.set(Some(combined_heat_capacity));
	}
	/// Returns a gas mixture that contains a given percentage of this mixture's moles; if this mix is mutable, also removes those moles from the original.
	pub fn remove_ratio(&mut self, mut ratio: f32, into: &mut GasMixture) {
		if ratio <= 0.0 {
			return;
		}
		if ratio >= 1.0 {
			ratio = 1.0;
		}
		into.copy_from_mutable(self);
		into.multiply(ratio);
		self.multiply(1.0 - ratio);
		self.garbage_collect();
		into.garbage_collect();
	}
	/// As remove_ratio, but a raw number of moles instead of a ratio.
	pub fn remove(&mut self, amount: f32, into: &mut GasMixture) {
		self.remove_ratio(amount / self.total_moles(), into);
	}
	/// Copies from a given gas mixture, if we're mutable.
	pub fn copy_from_mutable(&mut self, sample: &GasMixture) {
		if self.immutable && !sample.is_corrupt() {
			return;
		}
		self.mole_ids = sample.mole_ids.clone();
		self.moles = sample.moles.clone();
		self.temperature = sample.temperature;
		self.heat_capacities = sample.heat_capacities.clone();
		self.cached_heat_capacity
			.set(sample.cached_heat_capacity.get());
	}
	/// A very simple finite difference solution to the heat transfer equation.
	/// Works well enough for our purposes, though perhaps called less often
	/// than it ought to be while we're working in Rust.
	/// Differs from the original by not using archive, since we don't put the archive into the gas mix itself anymore.
	pub fn temperature_share(
		&mut self,
		sharer: &mut GasMixture,
		conduction_coefficient: f32,
	) -> f32 {
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
	pub fn temperature_compare(&self, sample: &GasMixture) -> bool {
		(self.get_temperature() - sample.get_temperature()).abs()
			> MINIMUM_TEMPERATURE_DELTA_TO_SUSPEND
			&& (self.total_moles() > MINIMUM_MOLES_DELTA_TO_MOVE)
	}
	/// Returns the maximum mole delta for an individual gas.
	pub fn compare(&self, sample: &GasMixture) -> f32 {
		let mut our_idx = 0;
		let mut sample_idx = 0;
		let mut ret: f32 = 0.0;
		for e in self
			.mole_ids
			.iter()
			.merge_join_by(sample.mole_ids.iter(), |a, b| a.cmp(b))
		{
			match e {
				Both(_, _) => {
					ret = ret.max(
						unsafe {
							self.moles.get_unchecked(our_idx)
								- sample.moles.get_unchecked(sample_idx)
						}
						.abs(),
					);
					our_idx += 1;
					sample_idx += 1;
				}
				Left(_) => {
					ret = ret.max(unsafe { *self.moles.get_unchecked(our_idx) });
					our_idx += 1;
				}
				Right(_) => {
					ret = ret.max(unsafe { *sample.moles.get_unchecked(sample_idx) });
					sample_idx += 1;
				}
			}
		}
		ret
	}
	/// Clears the moles from the gas.
	pub fn clear(&mut self) {
		if !self.immutable {
			self.mole_ids.set_all(false);
			self.mole_ids.clear();
			self.moles.clear();
			self.heat_capacities.clear();
			self.cached_heat_capacity.set(None);
		}
	}
	/// Resets the gas mixture to an initialized-with-volume state.
	pub fn clear_with_vol(&mut self, vol: f32) {
		self.temperature = 2.7;
		self.volume = vol;
		self.min_heat_capacity = MINIMUM_HEAT_CAPACITY;
		self.immutable = false;
		self.clear();
	}
	/// Multiplies every gas molage with this value.
	pub fn multiply(&mut self, multiplier: f32) {
		if !self.immutable {
			self.cached_heat_capacity
				.set(Some(self.heat_capacity() * multiplier)); // hax
			for amt in self.moles.iter_mut() {
				*amt *= multiplier;
			}
		}
	}
	/// Checks if the proc can react with any reactions.
	pub fn can_react(&self) -> bool {
		with_reactions(|reactions| reactions.iter().any(|r| r.check_conditions(self)))
	}
	/// Gets all of the reactions this mix should do.
	pub fn all_reactable(&self) -> Vec<ReactionIdentifier> {
		with_reactions(|reactions| {
			reactions
				.iter()
				.filter_map(|r| {
					if r.check_conditions(self) {
						Some(r.get_id())
					} else {
						None
					}
				})
				.collect()
		})
	}
	/// Adds heat directly to the gas mixture, in joules (probably).
	pub fn adjust_heat(&mut self, heat: f32) {
		let cap = self.heat_capacity();
		self.set_temperature(((cap * self.temperature) + heat) / cap);
	}
	/// Returns true if there's a visible gas in this mix.
	pub fn is_visible(&self) -> bool {
		self.enumerate()
			.any(|(i, &gas)| gas_visibility(i).map_or(false, |amt| gas >= amt))
	}
	/// A hashed representation of the visibility of a gas, so that it only needs to update vis when actually changed.
	pub fn visibility_hash(&self) -> u64 {
		use std::hash::Hasher;
		let mut hasher = std::collections::hash_map::DefaultHasher::new();
		for (i, &gas) in self.enumerate() {
			if gas_visibility(i).map_or(false, |amt| gas >= amt) {
				hasher.write(&[
					i as u8,
					FACTOR_GAS_VISIBLE_MAX.min((gas / MOLES_GAS_VISIBLE_STEP).ceil()) as u8,
				]);
			}
		}
		hasher.finish()
	}
	// Removes all zeroes from the gas mixture.
	fn garbage_collect(&mut self) {
		// this is absolutely just a copy job of the source for the rust standard library's retain
		let mut del = 0;
		let ones: Vec<usize> = self.mole_ids.iter_ones().collect();
		let len = self.moles.len();
		{
			for idx in 0..len {
				let amt = unsafe { *self.moles.get_unchecked(idx) };
				if !amt.is_normal() || amt <= GAS_MIN_MOLES {
					del += 1;
				} else if del > 0 {
					self.moles.swap(idx - del, idx);
					self.mole_ids.set(ones[idx], false);
					self.heat_capacities.swap(idx - del, idx);
				}
			}
		}
		if del > 0 {
			self.moles.truncate(len - del);
			self.heat_capacities.truncate(len - del);
		}
		// not recaching because the difference is, at literal most, on the order of 0.0001
	}
}

use std::ops::{Add, Mul};

/// Takes a copy of the mix, merges the right hand side, then returns the copy.
impl Add<&GasMixture> for GasMixture {
	type Output = Self;

	fn add(self, rhs: &GasMixture) -> Self {
		let mut ret = self.clone();
		ret.merge(rhs);
		ret
	}
}

/// Takes a copy of the mix, merges the right hand side, then returns the copy.
impl<'a, 'b> Add<&'a GasMixture> for &'b GasMixture {
	type Output = GasMixture;

	fn add(self, rhs: &GasMixture) -> GasMixture {
		let mut ret = self.clone();
		ret.merge(rhs);
		ret
	}
}

/// Makes a copy of the given mix, multiplied by a scalar.
impl Mul<f32> for GasMixture {
	type Output = Self;

	fn mul(self, rhs: f32) -> Self {
		let mut ret = self.clone();
		ret.multiply(rhs);
		ret
	}
}

/// Makes a copy of the given mix, multiplied by a scalar.
impl<'a> Mul<f32> for &'a GasMixture {
	type Output = GasMixture;

	fn mul(self, rhs: f32) -> GasMixture {
		let mut ret = self.clone();
		ret.multiply(rhs);
		ret
	}
}

pub enum GasMixtureAlloc {
	Arena(usize),
	Heap(GasMixture),
}

impl GasMixtureAlloc {
	pub fn with<T>(&self, f: impl FnOnce(&GasMixture) -> T) -> T {
		match self {
			Self::Arena(idx) => f(super::GAS_MIXTURES.get(*idx).unwrap()),
			Self::Heap(mix) => f(mix),
		}
	}
	pub fn with_mut<T>(&mut self, f: impl FnOnce(&mut GasMixture) -> T) -> T {
		match self {
			Self::Arena(idx) => f(super::GAS_MIXTURES.get_mut(*idx).unwrap()),
			Self::Heap(mix) => f(mix),
		}
	}
	pub fn copy_to_new(&self) -> GasMixture {
		match self {
			Self::Arena(_) => self.with(|gas| gas.clone()),
			Self::Heap(mix) => mix.clone(),
		}
	}
}

impl Drop for GasMixtureAlloc {
	fn drop(&mut self) {
		if let Self::Arena(idx) = self {
			super::GAS_MIXTURES.remove(*idx);
		}
	}
}

pub struct GasSummer {
	cur_ids: BitVec<Lsb0, u8>,
	cur_summed_counts: Vec<f64>,
	cur_summed_heat_cap: f64,
	cur_summed_heat: f64,
	cur_summed_vols: f64,
	cur_temp: f64,
}

impl GasSummer {
	pub fn merge(&mut self, gas: &GasMixture) {
		for e in gas.enumerate() {
			let (id, amt) = (e.0, *e.1 as f64);
			match get_bit_position(&self.cur_ids, id) {
				Ok(idx) => unsafe { *self.cur_summed_counts.get_unchecked_mut(idx) += amt },
				Err((idx, resize)) => {
					self.cur_summed_counts.insert(idx, amt);
					if resize {
						self.cur_ids.resize(id + 1, false);
					}
					self.cur_ids.set(id, true);
				}
			}
		}
		self.cur_summed_heat_cap += gas.heat_capacity() as f64;
		self.cur_summed_heat += gas.thermal_energy() as f64;
		self.cur_summed_vols += gas.volume as f64;
		self.cur_temp = self.cur_summed_heat / self.cur_summed_heat_cap;
	}
	pub fn copy_into(&self, gas: &mut GasMixture) {
		if gas.immutable {
			return;
		}
		let coeff = (gas.volume as f64) / self.cur_summed_vols;
		gas.clear();
		for (id, amt) in self.cur_ids.iter_ones().zip(
			self.cur_summed_counts
				.iter()
				.map(|amt| (amt * coeff) as f32),
		) {
			gas.set_moles(id, amt);
		}
		gas.set_temperature(self.cur_temp as f32);
	}
	pub fn copy_with_vol(&self, vol: f64) -> GasMixture {
		let coeff = vol / self.cur_summed_vols;
		GasMixture {
			mole_ids: self.cur_ids.clone(),
			moles: self
				.cur_summed_counts
				.iter()
				.map(|amt| (amt * coeff) as f32)
				.collect(),
			heat_capacities: self
				.cur_ids
				.iter_ones()
				.map(|id| gas_specific_heat(id))
				.collect(),
			temperature: self.cur_temp as f32,
			volume: vol as f32,
			min_heat_capacity: 0.0,
			immutable: false,
			cached_heat_capacity: Cell::new(None),
		}
	}
	pub fn return_pressure(&self) -> f32 {
		let moles = self.cur_summed_counts.iter().sum::<f64>();
		((moles * R_IDEAL_GAS_EQUATION as f64 * self.cur_temp as f64) / self.cur_summed_vols) as f32
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_merge() {
		let mut into = GasMixture::new();
		into.set_moles(0, 82.0);
		into.set_moles(1, 22.0);
		into.set_temperature(293.15);
		let mut source = GasMixture::new();
		source.set_moles(3, 100.0);
		source.set_temperature(313.15);
		into.merge(&source);
		// make sure that the merge successfuly moved the moles
		assert_eq!(into.get_moles(3), 100.0);
		assert_eq!(source.get_moles(3), 100.0); // source is not modified by merge
										/*
										make sure that the merge successfuly changed the temperature of the mix merged into
										test gases have heat capacities of 2,080 and 20,000 respectively, so total thermal energies of
										609,752 and 6,263,000 respectively once multiplied by temperatures. add those together,
										then divide by new total heat capacity:
										(609,752 + 6,263,000)/(2,080 + 20,000) =
										6,872,752 / 2,2080 ~
										311.265942
										so we compare to see if it's relatively close to 311.266, cause of floating point precision
										*/
		assert!(
			(into.get_temperature() - 311.266).abs() < 0.01,
			"{} should be near 311.266, is {}",
			into.get_temperature(),
			(into.get_temperature() - 311.266)
		);
	}
	#[test]
	fn test_remove() {
		// also tests multiply, copy_from_mutable
		let mut removed = GasMixture::new();
		removed.set_moles(0, 22.0);
		removed.set_moles(1, 82.0);
		let mut new = GasMixture::new();
		removed.remove_ratio(0.5, &mut new);
		assert_eq!(removed.compare(&new) >= MINIMUM_MOLES_DELTA_TO_MOVE, false);
		assert_eq!(removed.get_moles(0), 11.0);
		assert_eq!(removed.get_moles(1), 41.0);
		removed.mark_immutable();
		let mut new_two = GasMixture::new();
		removed.remove_ratio(0.5, &mut new_two);
		assert_eq!(
			removed.compare(&new_two) >= MINIMUM_MOLES_DELTA_TO_MOVE,
			true
		);
		assert_eq!(removed.get_moles(0), 11.0);
		assert_eq!(removed.get_moles(1), 41.0);
		assert_eq!(new_two.get_moles(0), 5.5);
	}
}
